# av-edge-plugin — NIST SP 800-171 Rev 2 control matrix

The edge plugin (`docs/edge-plan.md` milestones E4 "the plugin" and E6 "capture-only while
disconnected"): `crates/av-edge/src/plugin/` (the replay/signing library), `crates/av-edge/
src/buffer.rs` (the E6 durable edge buffer and its uplink driver), the signing/verification/
chain/identity machinery those two depend on (`crates/av-edge/src/{sign,verify,hash,chain,
policy,identity}.rs`), `crates/av-ingest-client` (the wire client library plus the
`av-edge-plugin` binary, `src/bin/av-edge-plugin.rs`), and `services/edge-plugin/` (the
container: `Dockerfile`, `build-image.sh`). This maps that code to CMMC Level 2 (NIST SP
800-171 Rev 2, 110 requirements / 14 families), in the format `docs/compliance/av-ingest/
control-matrix.md` and `docs/compliance/av-dynamics-service/control-matrix.md` use (family,
requirement ID, requirement, implementation `file:function`, evidence command, the
Met / Partial / Inherited / Gap legend, a scope section, a deficiency list), extended with a
short "numbers" section per this task's own brief.

```{admonition} Not a certification
This is engineering documentation, not a C3PAO assessment or an attestation. CMMC Level 2
is a property of the accreditation boundary as a whole, not of one component. **Most rows
below are Gap, Partial or Inherited — that is the honest state of E4/E6 today (ADR-004: "a
control matrix with mostly `Gap` rows is acceptable in P0; a component without one is
not"), not a defect in this document.** Two rows below (AU 3.3.8, CM 3.4.2) are genuinely
`Met`; several others that could plausibly have been written up as `Met` are marked
`Partial` deliberately — see "Rows downgraded from Met" in this document's own worker
report for why.
```

## Legend

| Mark | Meaning |
|---|---|
| Met | Addressed by this component's own code, verifiable by the evidence command in that row. |
| Partial | Something real exists but depends on deployment/configuration this component does not itself control, or covers only part of the requirement. |
| Inherited | Satisfied by the environment/organization (host, network, IdP, enclave), or by a sibling component this one depends on but does not itself implement. |
| Gap | Not implemented anywhere in this component today. See [Deficiencies](#deficiencies). |

## Scope

This component is a **library plus a binary plus a container** — it exposes no gRPC
service and no HTTP endpoint of its own; it only ever *dials out*, once, to `--endpoint`.

- `crates/av-edge/src/plugin/mod.rs` — `PluginConfig` (every replay knob, `validate`,
  `config_hash`, `manifest`, `batch_provenance`), `PortTrafficSource` (decodes a run's
  `PortTrafficLog` sidecar through one declared `PacketCodec`), `BatchingRule`/`Pacing`
  (pure policies), `BatchBuilder`/`BatchIdentity` (turns a source's groups into signed,
  chained `MeasurementBatch`es), and `verify_port_traffic_log` (hash-verifies the sidecar
  bytes before a single one is decoded).
- `crates/av-edge/src/plugin/packet.rs` — a thin adapter over `av_codec::decode_packet`
  (question 205's extraction of the CCSDS decoder out of `av-kernel`, so this crate depends
  on neither `gmat-sys` nor any transport crate).
- `crates/av-edge/src/buffer.rs` — **E6, new this round**: `EdgeBuffer` (a durable,
  append-only, hash-chained, file-backed log of already-signed batches plus a persisted ack
  watermark) and `BatchSink`/`UplinkDriver` (buffer-while-down, drain-before-send, one
  commit per uninterrupted drain).
- `crates/av-edge/src/{sign,verify,hash,chain,policy,identity}.rs` — ECDSA P-384
  signing/verification over the system OpenSSL, the canonical SHA-256 hash-chain
  definition, the per-producer chain verifier and pure chain walker, the label/clearance
  policy, and seccert-leaf identity verification against the two-tier Root. All five are
  shared with `crates/av-ingest`'s own control matrix; this document scores only what the
  *plugin's own call path* actually exercises them for.
- `crates/av-ingest-client/src/lib.rs` — `EdgeIngestClient::connect_plaintext` (plaintext
  h2c, refuses a non-loopback address before ever dialing) and `is_loopback_address`.
- `crates/av-ingest-client/src/bin/av-edge-plugin.rs` — the plugin binary: loads and
  hash-verifies the fixture, validates the config, builds and signs the batch chain,
  connects (plaintext-loopback or, given `--endpoint https://...`, mTLS through
  `av_grpc::tls::connect`), announces, paces, submits, and prints one JSON summary line.
  This is the **only** place in this whole path that ever calls `std::thread::sleep`.
- `services/edge-plugin/Dockerfile` / `build-image.sh` — packages the plugin binary as
  `av-edge-plugin:local`, a self-contained, offline replay of the demo fixture, pinned by
  digest, labelled for prune-before-create.
- `tests/test_edge_plugin_container.py`, `crates/av-ingest-client/tests/
  av_edge_plugin_binary.rs`, `crates/av-ingest/tests/{e6_disconnect,e6_wire_disconnect}.rs`
  — what is actually proven, and how (cited row by row below).

**Not in scope for this document**: `crates/av-ingest`'s own server, admin surface and
ledger (`docs/compliance/av-ingest/control-matrix.md` already scores those), and
`crates/av-track`/`spoore` (E5, downstream of the ingest).

## Numbers this document's evidence rests on

Every number below was reproduced by this task, not copied from `docs/edge-plan.md`
unchecked — see the worker report for the exact command and printed output.

- **Pinned chain head over 900 batches**:
  `d1d80d0b6cc9228aaa7479864b8a89f18be88382fb3772bd3c4cc3d7c2dc2698` — the `batch_hash` of
  the 900th, last batch `BatchBuilder::build_batches` produces from the committed
  `demo_ground_segment` fixture, asserted by `crates/av-edge/tests/plugin_replay.rs::
  batch_count_and_chain_head_are_pinned_for_the_demo_drm` and independently reproduced by
  the built binary (`crates/av-ingest-client/tests/av_edge_plugin_binary.rs`) and by the
  container test reading it back from the ingest's own evidence surface
  (`tests/test_edge_plugin_container.py`).
- **The fixture's `port_traffic_hash`**:
  `c548a78c80954c2a6a159d2b27df10e9f55213e2bed2a1628332b31a63e93dc7` — what
  `verify_port_traffic_log` (`crates/av-edge/src/plugin/mod.rs`) checks
  `port_traffic.pb`'s bytes against before decoding a single record
  (`crates/av-edge/tests/fixtures/ground_segment/README.md`).
- **E6's byte-identical-log result**: `crates/av-ingest/tests/e6_disconnect.rs::
  the_ingests_log_is_byte_identical_across_an_interrupted_and_uninterrupted_run` and
  `crates/av-ingest/tests/e6_wire_disconnect.rs::
  the_real_wire_survives_a_client_side_disconnect_and_replays_byte_identically` both assert
  that a run interrupted mid-stream and later drained through `EdgeBuffer`/`UplinkDriver`
  produces a partition log **byte-for-byte identical** to an uninterrupted run over the
  same already-signed batches — the property that makes the ingest's duplicate counter,
  not data loss or double-counting, the observable effect of a real disconnect.
- **Decoded-position-vs-truth-trajectory deviation**: `0` m over all 900 epochs
  (`crates/av-edge/tests/plugin_replay.rs::
  decoded_positions_match_the_flight_instances_truth_trajectory_within_tolerance`, printed
  directly rather than only asserted) — not a security number, but the number that
  underwrites every other pinned value in this section: it is what proves the plugin
  decodes what the DRM actually produced, not merely something self-consistent.

## 3.1 Access Control (AC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.1.1 / 3.1.2 | Limit system access to authorized users/processes | Gap | N/A — this component has no listener of its own to protect at all (`av-edge-plugin` only ever dials `--endpoint`, once, per `crates/av-ingest-client/src/bin/av-edge-plugin.rs::Client::connect`); the *ingest's* own caller authentication is `docs/compliance/av-ingest/control-matrix.md`'s AC 3.1.1/3.1.2 row, not this one | N/A |
| 3.1.3 | Control the flow of CUI | Partial | `PluginConfig::label_bytes`/`clearance` (`crates/av-edge/src/plugin/mod.rs`) declares the exact label every batch this config signs will carry (`PluginConfig::manifest`, `BatchBuilder::build_batches`) — but nothing in this component's own code checks that the declared label's rank does not exceed the declared clearance; that check is `crate::policy::ProducerPolicy::classify_label`, run only at the ingest boundary (a sibling crate) when a batch actually arrives, never by the plugin before it signs and sends one | `cargo test -p av-edge --test plugin_replay manifest_and_batches_are_derived_from_the_same_config -- --nocapture` (label consistency across manifest/batches); `grep -n "clearance" crates/av-edge/src/plugin/mod.rs` shows `clearance` is validated non-empty (`PluginConfig::validate`) but never compared against a ladder anywhere in this file |
| 3.1.5 | Least privilege | Gap | ADR-004 specifies the plugin runs as "a rootless podman container (Quadlet-managed) on a UBI9 FIPS base image ... read-only root, dedicated user, seccomp, no new privileges." `services/edge-plugin/Dockerfile` implements **none** of that: it is built and run with plain `docker`, not podman; its base is `debian:bookworm-slim`, not a UBI9 FIPS image; it sets no `USER` (the process runs as the image default, root); and neither the Dockerfile nor `build-image.sh` nor `tests/test_edge_plugin_container.py`'s own `docker run` invocations pass `--read-only`, a seccomp profile, or `--security-opt=no-new-privileges`. See [Deficiencies](#deficiencies) | `grep -n "USER \|read-only\|seccomp\|no-new-privileges" services/edge-plugin/Dockerfile services/edge-plugin/build-image.sh tests/test_edge_plugin_container.py` (no output) |
| 3.1.12 / 3.1.13 | Control & encrypt remote access | Partial | `EdgeIngestClient::connect_plaintext`/`is_loopback_address` (`crates/av-ingest-client/src/lib.rs`) refuse a non-loopback address **before ever dialing** — real, tested client-side policy. The "encrypt" half exists only on the separate `--endpoint https://...` path (`av_grpc::tls::connect`, `crates/av-ingest-client/src/bin/av-edge-plugin.rs::Client::connect`); no test in this repository proves the `av-edge-plugin` binary completing a real mTLS handshake end to end — see SC 3.13.8 below | `cargo test -p av-ingest-client --lib connect_plaintext_refuses_a_non_loopback_address_without_dialing -- --nocapture` |
| 3.1.20 | Control connections to external systems | Partial | `services/edge-plugin/Dockerfile` packaged with `--network none` (deny-all) or a labelled `--internal` bridge sharing the ingest's own network namespace (allowed-endpoint) is this component's own, measured contribution — but which network mode a `docker run` actually uses is an operator choice the image itself does not enforce (nothing about the image prevents running it with default bridge networking and published ports) | `.venv/bin/python -m pytest -q -rs tests/test_edge_plugin_container.py` (currently skips visibly on this host — see [Deficiencies](#deficiencies), item 8 — but see the worker report for the round-2 manager's own prior successful run recorded in `docs/edge-plan.md`) |
| 3.1.22 | Control publicly-posted content | Inherited | This component posts nothing publicly | N/A |
| 3.1.4/3.1.6–3.1.11/3.1.14–3.1.19/3.1.21 | Separation of duties, session lock, MFA-gated remote access, mobile/wireless, etc. | Inherited | Environment/IdP responsibilities; not applicable to an outbound-only client | N/A |

## 3.3 Audit and Accountability (AU)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.3.1 | Create and retain audit records | Partial | `EdgeBuffer::append` (`crates/av-edge/src/buffer.rs`) durably records every signed batch while the uplink is down, chained and replayed in exact append order (`EdgeBuffer::replay_from`) — but this is a **transient, capture-only buffer**, not this platform's system of record (ADR-004's own "capture-only when disconnected: ... no tracking or authority runs at the edge"); the ledger of record is `av-ingest`'s own `PartitionLog`, already scored `Met` in `docs/compliance/av-ingest/control-matrix.md`. Nothing in this crate rotates, archives, or bounds `EdgeBuffer`'s own file size once a drain succeeds and the watermark advances | `cargo test -p av-edge --lib buffer::tests::append_then_replay_from_returns_every_record_in_append_order -- --nocapture` and `cargo test -p av-ingest --test e6_disconnect buffering_survives_a_multi_hour_disconnection_with_no_loss -- --nocapture` |
| 3.3.2 | Trace actions to individual users/processes | Partial | Every batch traces to `MeasurementBatch.producer_id` and, when a `leaf_fingerprint_sha256` is configured, to `signer_cert_sha256` (`PluginConfig::manifest`, `BatchBuilder::build_batches`) — but there is no human principal anywhere on this path, and E1's own no-certificate path leaves `signer_cert_sha256` empty | `cargo test -p av-edge --test plugin_replay batch_count_and_chain_head_are_pinned_for_the_demo_drm -- --nocapture` |
| 3.3.4 | Alert on audit logging failure | Partial | `EdgeBuffer::append`/`persist_ack_watermark` return a typed `Result<_, BufferError>` on any I/O failure, propagated through `UplinkDriver::step` via `DriverError::Buffer` — fail-loud to the caller, never silently swallowed — but nothing in this component forwards that failure to an operator/SIEM | N/A (code inspection of `crates/av-edge/src/buffer.rs::EdgeBuffer::append`'s `?` propagation and `DriverError::Buffer`'s `#[from]`) |
| 3.3.5 / 3.3.6 | Correlate and report audit review | Gap | No correlation tooling of this component's own beyond `EdgeBuffer::record_count`/`ack_watermark`/`tip_hash` | N/A |
| 3.3.7 | Authoritative, time-synced timestamps | Gap | Every "now" this component ever sees (`UplinkDriver::step`'s `now_tai_ns`, `Pacing::due_at`'s epochs) is a plain `i64` the caller supplies — no live clock read anywhere (question 199); the plugin binary's own `main` reads real wall-clock time only through `std::time::Instant` for pacing *duration*, never as a timestamp recorded on any batch | `grep -n "Instant::now\|SystemTime::now\|sleep" crates/av-edge/src/buffer.rs` (the two hits are the module doc's own quoted grep, not code) |
| 3.3.8 | **Protect audit information from unauthorized access/modification** | Met | Two independent, real mechanisms: (1) `EdgeBuffer`'s own SHA-256 record-hash chain (`crates/av-edge/src/buffer.rs::EdgeBuffer::append`/`replay_from`/`scan_frames`, byte-for-byte the same framing as `PartitionLog`) detects a torn or tampered buffer file; (2) independently, every batch's own ECDSA P-384 signature and body hash (`crate::sign::sign_batch_with_signer`, `crate::verify::verify_batch`) protect the signed content itself, regardless of the buffer. This is real integrity-at-origin *and* a non-repudiation-shaped property (a batch cannot be altered after signing without invalidating the signature) — but `replay_from`'s own `DigestMismatch` branch (a record tampered with *after* being appended, as opposed to a torn tail at `open`) has no dedicated test in this crate's own suite; see [Deficiencies](#deficiencies) | `cargo test -p av-edge --lib buffer::tests::open_recovers_from_a_torn_final_record_and_reports_it buffer::tests::a_corrupt_length_ack_file_is_defaulted_to_zero_and_reported -- --nocapture` and `cargo test -p av-edge --lib sign:: verify:: -- --nocapture` |
| 3.3.9 | Limit audit management to a subset of privileged users | Gap | Nothing gates who may open, append to, or replay an `EdgeBuffer`, or call `verify_port_traffic_log` — this is a library; any caller of its public API can do all three | N/A |

## 3.4 Configuration Management (CM)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.4.1 | Baseline configuration & inventory | Partial | `Cargo.lock` pins every dependency; the repo-root `deny.toml` bans forbidden crypto crates and enforces a license allow-list. **As of this task, `cargo deny check bans` fails workspace-wide** — six spoore ML/engine crates (`spoore-assoc`/`spoore-engine`/`spoore-math`/`spoore-ml`/`spoore-models`/`spoore-tree`) lack `publish = false` (question 205 ruling 7 / `docs/edge-plan.md` round-2 open item 2) — but **none of those six sit on this component's own dependency path**: `cargo tree -p av-ingest-client` pulls in only `spoore-cdm`, which passes the wildcard check cleanly. The failure is real, workspace-wide, and not this component's to fix; `cargo deny check advisories` (below) is the correct per-component gate and is clean. No CycloneDX/SPDX SBOM, and no image-layer scan of `av-edge-plugin:local`, exists | `cargo tree -p av-ingest-client` and `cargo deny check advisories` (both run; see worker report) |
| 3.4.2 | Enforce security configuration settings | Met | `PluginConfig::validate` (`crates/av-edge/src/plugin/mod.rs`) refuses every empty required field, an empty `component_fields`, a `noise_r` that is not exactly `component_fields.len()^2` entries or fails `av_cdm::covariance::check_spd_row_major`, an undecodable `codec_bytes`/`label_bytes`, a `BatchingRule::PerNMeasurements(0)`, and a non-finite/non-positive `Pacing::RealTime` scale — nine independently-typed refusals, run before this config is ever handed to `PortTrafficSource::from_log` or a `BatchBuilder` | `cargo test -p av-edge --test plugin_replay validate_refuses_a_noise_matrix_that_is_not_spd validate_refuses_a_zero_sized_batching_rule -- --nocapture` |
| 3.4.6 | Least functionality | Met | `crates/av-ingest-client/src/bin/av-edge-plugin.rs::parse_args` matches exactly the eight documented flags and refuses (`"unrecognized argument"`) anything else; the binary opens exactly **one** outbound connection to `--endpoint` for the whole run (`Client::connect`/`run`) — no second endpoint, no telemetry call, confirmed by reading the code, not assumed (`tests/test_edge_plugin_container.py`'s own module doc makes the identical claim and is what this row's evidence command re-derives) | `grep -n "unrecognized argument" crates/av-ingest-client/src/bin/av-edge-plugin.rs` |
| 3.4.7 | Restrict nonessential programs/ports | Met (vacuous) | Zero listeners — `av-edge-plugin` is an outbound-only client | `grep -n "TcpListener\|::bind(\|listen(" crates/av-ingest-client/src/bin/av-edge-plugin.rs` (no output) |

## 3.5 Identification and Authentication (IA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.5.1 / 3.5.2 | Identify and authenticate users/processes | Partial | `crate::identity::verify_identity`/`TrustAnchors` (`crates/av-edge/src/identity.rs`) is real, tested, seccert-leaf-against-the-Root verification with an injected clock — but **nothing in `av-edge-plugin`'s own binary calls it** (`grep -n "identity" crates/av-ingest-client/src/bin/av-edge-plugin.rs` finds nothing at all). The plugin's own transport authentication is delegated entirely to the mTLS front (`--client-cert`/`--client-key`, presented but never verified by this component) and to `av-ingest`'s own `Announce`-time checks (round 2 decision 1, ratified by question 205(1): nginx runs `ssl_verify_client optional_no_ca`, so `av-ingest`, not nginx and not this plugin, is the enforcement point) | `cargo test -p av-edge --test identity -- --nocapture` (proves the mechanism works; does not prove the plugin binary uses it) and `grep -n "identity" crates/av-ingest-client/src/bin/av-edge-plugin.rs` (no output) |
| 3.5.3 | MFA for privileged/remote access | Inherited | IdP responsibility; not applicable to a headless plugin process | N/A |
| 3.5.10 | Cryptographically-protected passwords/secrets | Partial | This component's signing key and (for the mTLS path) client key are never generated, embedded, or baked into the image — `services/edge-plugin/Dockerfile`'s own header comment states this explicitly, and both are supplied only at `docker run` time as bind-mounted files named by `--signing-key`/`--client-key`. **Still Partial, not Met**: nothing in this crate encrypts, wraps, or otherwise protects those PEM files at rest — a private key sitting on the host or in a bind-mounted volume is protected only by whatever filesystem permissions its containing directory has | `grep -n "signing-key\|client-key" services/edge-plugin/Dockerfile` (absent from every `COPY`/`ENTRYPOINT` line — only ever named as a trailing `docker run` argument) |

## 3.13 System and Communications Protection (SC)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.13.1 / 3.13.5 | Boundary protection / subnetwork separation | Partial | `tests/test_edge_plugin_container.py::test_network_none_denies_everything_and_the_internal_network_delivers_batches_to_a_real_ingest` measures, not merely asserts, both halves: a bare `--network none` container fails every non-loopback connect attempt immediately with a typed `ENETUNREACH`-shaped error (never a hang), and a labelled `--internal` bridge with the plugin joined to the ingest's own network namespace (`--network container:<ingest>`) has no route off the host either. Real and measured — but see AC 3.1.5 above: no seccomp, no read-only root, no dedicated non-root user, and which network mode is actually used is an operator choice, not something the image enforces on itself | `.venv/bin/python -m pytest -q -rs tests/test_edge_plugin_container.py` (skips visibly on this host today — image not built; see [Deficiencies](#deficiencies)) |
| 3.13.6 | Deny network traffic by default | Partial | `EdgeIngestClient::connect_plaintext`/`is_loopback_address` refuse a non-loopback address before ever dialing (real, client-side, tested) — but this is "deny by policy in this one code path", not a network-layer default the image itself imposes; nothing prevents running the built image with ordinary bridge networking and published ports | `cargo test -p av-ingest-client --lib connect_plaintext_refuses_a_non_loopback_address_without_dialing -- --nocapture` |
| 3.13.8 | Encrypt CUI in transit | Gap (for the tested path) | `EdgeIngestClient::connect_plaintext` speaks plain HTTP/2 (h2c) — **zero encryption**, by design, for local-subprocess/no-front use only (this crate's own module doc). The alternative `--endpoint https://...` path (`av_grpc::tls::connect`) is real code, reused from `av-ingest-mtls-client`'s own precedent — but **no test in this repository proves the `av-edge-plugin` binary completing an actual mTLS handshake**: `crates/av-ingest-client/tests/av_edge_plugin_binary.rs` exercises the plaintext path only, and `tests/test_edge_plugin_container.py`'s own `https://` use is deliberately built to prove `--network none` blocks the *attempt* (the connection never reaches a handshake at all). Confidentiality in transit for this component, as committed and tested, is honestly `Gap`; the front (nginx mTLS) is what would provide it, and that has not been proven against this specific binary this round | `cargo test -p av-ingest-client --test av_edge_plugin_binary -- --nocapture` (plaintext path only — passes) |
| 3.13.11 | **Use FIPS-validated cryptography** | Gap | Same OpenSSL linkage every sibling crate shares (`openssl::sha::sha256`/ECDSA P-384 via `crate::hash`/`crate::sign`/`crate::verify`) — correct *algorithm*, no FIPS-validated module detected as present on this host (`crates/av-dynamics-service/src/fips.rs::detect`'s own finding, which this component's OpenSSL linkage shares; this component runs no equivalent detection of its own) | `cargo test -p av-dynamics-service --lib fips::tests -- --nocapture` |
| 3.13.15 | Protect authenticity of comms sessions | Gap (for the tested path) | No session/token concept on the plaintext path at all — anything able to reach the loopback address can pose as either end; the mTLS path would provide this but is unproven for this binary, as SC 3.13.8 above | N/A |

## 3.14 System and Information Integrity (SI)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.14.1 | Identify/correct flaws timely | Partial | `cargo deny check advisories` (RustSec DB) covers this component's own dependency tree; nothing schedules that on a cadence | `cargo deny check advisories` |
| 3.14.6 (input validation) | Monitor for attacks / validate input | Met | Every field this component's decisions depend on is validated before being trusted: `PluginConfig::validate` (nine typed refusals, CM 3.4.2 above), `plugin::packet::decode_numeric_fields` (`crates/av-edge/src/plugin/packet.rs`: header length, APID range/match, command-codec refusal, field extent, unsupported field type — eight typed `PacketError` variants, never a silent drop), and `PortTrafficSource::from_log` (`MissingComponentField` rather than a default/zero value) | `cargo test -p av-edge --lib plugin::packet:: -- --nocapture` (6 tests) |
| 3.14.6 (buffer integrity) | Detect corruption of stored data | Met | `EdgeBuffer::open`'s `scan_frames` distinguishes a torn trailing record (an interrupted append — reported and truncated away) from the general shape a corrupted middle record would take (a `DigestMismatch`/`Decode` error at `replay_from`, never silently repaired or skipped) | `cargo test -p av-edge --lib buffer::tests::open_recovers_from_a_torn_final_record_and_reports_it -- --nocapture` |

## 3.11 Risk Assessment (RA)

| ID | Requirement | Status | Implementation | Evidence |
|---|---|---|---|---|
| 3.11.2 | Scan for vulnerabilities | Partial | `cargo deny check advisories` covers this component's Rust dependency tree; **no image scan of `av-edge-plugin:local`** exists anywhere in this repository (no trivy/grype/equivalent), and this component is the only one in this document's family of sibling crates that actually ships a container image, so this gap is this component's own, not merely inherited | `cargo deny check advisories` |

## Inherited wholesale (not this component's responsibility)

Matching the sibling matrices' own posture for families this component has no material to
implement:

| Family | IDs | Status | Notes |
|---|---|---|---|
| AT (Awareness & Training) | 3.2.1–3.2.3 | Inherited | Organizational training |
| IR (Incident Response) | 3.6.1–3.6.3 | Inherited | Organizational process; this component's contribution is the fail-loud `BufferError`/typed-refusal surface above (AU) |
| MA (Maintenance) | 3.7.1–3.7.6 | Inherited | Host/equipment maintenance |
| MP (Media Protection) | 3.8.1–3.8.9 | Inherited | **Split finding, stated plainly per this task's own brief**: `EdgeBuffer` protects the **integrity** of what it stores at rest (the per-record SHA-256 chain, AU 3.3.8 above) but provides **zero confidentiality at rest** — every signed batch sits as plaintext protobuf bytes on the edge host's local filesystem between being buffered and being successfully drained, protected only by whatever filesystem permissions its containing directory has (`EdgeBuffer::open`/`append` never `chmod` the file). Confidentiality-at-rest for this component is a `Gap`, not an `Inherited` mitigation — nothing downstream (the host, the container) is documented anywhere in this round as providing it either |
| PS (Personnel Security) | 3.9.1–3.9.2 | Inherited | Screening/transfer, organizational |
| PE (Physical Protection) | 3.10.1–3.10.6 | Inherited | Data-center/facility controls |
| CA (Security Assessment) | 3.12.1–3.12.4 | Inherited | SSP, POA&M, continuous monitoring; this document is an input to those, not a substitute |

## Deficiencies

Ranked by what a reviewer would flag first:

1. **The shipped container does not match ADR-004's own design for a plugin** (AC 3.1.5, SC
   3.13.1). ADR-004: "a rootless podman container (Quadlet-managed) on a UBI9 FIPS base
   image, `--network none` plus an explicit allow-list ..., read-only root, dedicated user,
   seccomp, no new privileges." `services/edge-plugin/Dockerfile` and `build-image.sh` use
   plain `docker` (not podman/Quadlet), a `debian:bookworm-slim` base (not UBI9 FIPS), and
   set no `USER`, no `--read-only`, no seccomp profile, and no
   `--security-opt=no-new-privileges` anywhere — confirmed by grep, not assumed. The
   `--network none`/`--internal` half of ADR-004's design **is** real and measured
   (SC 3.13.1's Partial row); the container-hardening half is not implemented at all. This
   is a real defect against ADR-004's own stated design, found while reading the Dockerfile
   for this document, not a theoretical gap.
2. **Confidentiality in transit is unproven for this binary** (SC 3.13.8/3.13.15). The
   plaintext path this repository's own tests actually exercise
   (`crates/av-ingest-client/tests/av_edge_plugin_binary.rs`) has zero encryption by design.
   The mTLS alternative exists as code (`av_grpc::tls::connect`) but no committed test
   proves `av-edge-plugin` completing a real handshake — `tests/
   test_edge_plugin_container.py`'s own `https://` use only proves `--network none` blocks
   the connection *attempt*, deliberately never reaching a handshake. Confidentiality in
   transit for this component, as it stands today, is real for the front (once deployed and
   proven the way `services/gmat-service`'s nginx front is proven against
   `av-dynamics-service`) but not demonstrated for this plugin's own binary this round.
3. **No caller/process identity check anywhere on the plugin's own call path** (IA
   3.5.1/3.5.2). `crate::identity`'s verification code is real and independently tested (11
   passing tests, `crates/av-edge/tests/identity.rs`) but is never invoked by
   `av-edge-plugin`'s own binary — its own transport identity is entirely delegated to the
   mTLS front and to `av-ingest`'s enforcement, per round 2's decision 1 (question 205(1)).
4. **`EdgeBuffer` provides integrity, not confidentiality, at rest** (MP, above). Every
   signed batch sits as plaintext on the edge host's local disk while buffered, for however
   long the uplink stays down (E6's own charter: "hours"). Nothing in this repository
   documents an at-rest encryption mitigation for this specific window.
5. **No FIPS-validated cryptographic module** (SC 3.13.11), inherited directly from the same
   OpenSSL linkage every sibling crate already documents lacking one on this host.
6. **`cargo deny check bans` currently fails workspace-wide** (CM 3.4.1, RA 3.11.2). Six
   spoore ML/engine crates lack `publish = false` (question 205 ruling 7 / `docs/
   edge-plan.md` round-2 open item 2) — not rooted in this component's own dependency path
   (`spoore-cdm`, the one spoore crate this component's tree pulls in, passes cleanly), but
   the workspace-wide gate this component also runs under is not clean today.
7. **No access control on `EdgeBuffer` or `verify_port_traffic_log`** (AU 3.3.9). Any caller
   of this library's public API can open, append to, or replay a buffer, or verify (and
   thereby decode) a port-traffic log — appropriate for a library embedded in a trusted
   process, a real gap if this crate is ever linked into something that needs to gate who
   may call it.
8. **Container image availability on this host is not guaranteed** (RA 3.11.2's scope note;
   question 196(d)). `av-edge-plugin:local` has been observed deleted mid-run by Colima's
   own kubelet image garbage collector once the VM disk passes its high-water mark — closed
   by the lead as a definitive root cause, not a hypothesis. `tests/
   test_edge_plugin_container.py` skips visibly rather than failing when the image is
   absent, and this document's own reproduction of the container test skipped for exactly
   that reason on this host at the time of writing. Rebuild-on-demand
   (`services/edge-plugin/build-image.sh`) with a visible skip is the standing mitigation,
   not a fix — no image scan (RA 3.11.2) exists either, compounding the same row.
9. **`EdgeBuffer`'s own `replay_from`-time tamper detection (`DigestMismatch`) has no
   dedicated test** (AU 3.3.8's scope note). The mechanism exists in shipped code and
   mirrors `PartitionLog`'s already-tested equivalent, but unlike the torn-tail-at-`open`
   path (which is tested), no test in this crate's own suite appends a record, corrupts it
   on disk, and asserts `replay_from` reports `BufferError::DigestMismatch`.
10. **The buffer file has no retention, rotation, or at-rest access-control policy of its
    own** (AU 3.3.1's retention half, MP above). It grows without bound until a drain
    succeeds and is protected only by whatever filesystem permissions its containing
    directory happens to have.
