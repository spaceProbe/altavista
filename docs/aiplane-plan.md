# AI plane and command authority: plan

Requested by the user 2026-09-12 as one of two parallel tracks (the other is
`edge-plan.md`). Decisions in `open-questions.md` question 201. This is P4 in
`architecture.md` ("a model proposes, a human authorizes, a simulated asset acts, replay
reproduces the trail") and the AI-plane and command-authority halves of ADR-004.

## Goal

The command state machine enforced by a service: policy at CHECKED, a role-gated human at
AUTHORIZED with MFA for hazardous classes and time-limited delegations, dispatch to the
simulated asset with deadline, idempotency key and ack levels, every transition an event
with its principal; a model sidecar that can only propose, through a read-only, label-aware
data gateway; a console in the execution profile; a replay that reproduces the decision
trail including what the model saw.

## What exists

- `command.proto`: `Command`, `CommandState`, `CommandTransition` (principal, reason, ack
  level, delegation id), `AckLevel`, `CommandProposal`; `envelope.proto`: `Envelope`
  (carried, none enabled, question 53); `Label` and `Provenance` in core.proto.
- The state machine runs end to end in simulation (M25.2, M25.2b): `crates/av-kernel/src/
  drm/command.rs` (`propose_check_authorize`, `dispatched_event`, `acked_event`, sequence
  numbers, CCSDS command and ack codecs), telecommands through the real cFS command path
  (M25.2b, `gmat_command.rs`), transitions as `EVENT_KIND_COMMAND_TRANSITION` events, the
  replay binding and bit-identical replay (M25.4), command transitions on the viewer
  timeline.
- Hash-chained evidence, `/admin/api/evidence`, the FIPS module and control matrices in
  `crates/av-dynamics-service`; the tiling layout and panels in `web/js/layout/` and
  `web/js/panels/`; profiles under `profiles/` (`execution.yaml`).
- Not yet: a command service, any policy evaluation, any principal, roles, MFA or
  delegations, expiry, a data gateway, an MCP surface, a proposing model, a console, an
  evidence topic for proposals.

## Isolation

The AI-plane team works in the git worktree `/Users/probe/code/AltaVista-aiplane` on branch
`aiplane` (from `develop`). New code lives in new crates `crates/av-command` (the service and
the policy and authorization library), `crates/av-gateway` (the read-only data gateway and
its MCP server) and `crates/av-proposer` (the first model sidecar), a new proto file
`proto/altavista/v1/authority.proto` (this track owns it: policy decisions, delegations,
roles, the proposal evidence record; changes to `command.proto` and the other shared files
must be additive and pass `buf breaking`), new modules under `altavista/command/` and
`web/js/panels/`. It consumes `av_kernel` as a library, may add a binding kind for the
service in the executor only through the existing binding registry, and does not edit the
kernel's router or fault modules. The edge team works in its own worktree at the same time;
the tracks share nothing but `develop`, which the lead merges into both between rounds and
each back after acceptance. The verification clone for lead gates is
`/Users/probe/code/AltaVista-verify`.

## Rules that bind this track

Every standing rule in `teamlog/2026-09-02-team-1.md` and `open-questions.md` applies:
questions 148 (an exit code is not evidence), 154 (no network at test time), 156 (label
every test container and image; prune by label before creating), 157 (status sections in
this plan, not REPORT.md; about 300 tool uses per task), 172 (warm up fresh binaries), 194
(docker-gated tests skip visibly or run for real), 199 (no test mutates the process
environment), the crypto rule of ADR-004 (SHA-256 only, the system OpenSSL only, no `ring`,
no bundled crypto; `cargo deny` enforces the bans), the contention rule (check `ps` before a
heavy run; one heavy job per host; two tracks share this host with the lead), and question
53 (propose-only: nothing in this track enables an envelope). Root-cause every defect or
record why there is no path.

## Milestones

**A1 The command service (`crates/av-command`).** The state machine as a library with one
transition function per edge and a typed refusal for every illegal edge, persisted to a
durable file-backed log chained per partition (the entity), the log being the ledger
(ADR-004); a gRPC service over the same OpenSSL-backed tonic `av-grpc` uses, exposing
propose, check, authorize, dispatch, ack and query; `/admin/api/evidence` and ledger
`verify`. Policy at CHECKED evaluated from Rego (question 201: the `regorus` crate with its
crypto built-ins disabled, policies as `.rego` files declared by the profile, OPA-compatible
so a deployed OPA can replace the in-process evaluator) over an input of label, entity,
command class, hazardous flag, envelope id and the rate of recent commands per class; the
decision id and the policy hash go into the transition's reason. Tests: every legal path and
every illegal edge; a policy fixture that admits one class, rejects another, rate-limits a
third and refuses any envelope; the decision reproduced from the log with the same policy
hash; two runs over the same inputs give byte-identical logs.

**A2 Human authorization.** Principals from OIDC tokens with the secsso claims contract
(`sub`, `groups`, `amr`, `acr`; a local test issuer signs tokens with an OpenSSL key, and the
service verifies with the issuer's public key, so tests need no Authentik). Role-gated
authorization per command class from a profile table; hazardous classes require an MFA
claim; time-limited delegations (`authority.proto`) with enforced expiry evaluated against
the injected clock; every transition carries its principal and delegation; every transition
is written as a syslog-format audit line to a configured sink (the SIEM export of question
54) and retained in the ledger. Tests: the right role authorizes, the wrong role is
refused with the reason, hazardous without MFA is refused, an expired delegation is refused
at the boundary second, the audit line for each is exact.

**A3 Dispatch to the simulated asset.** The service dispatches AUTHORIZED commands into the
kernel's existing telecommand path (M25.2b) through a binding, with `not_before`, deadline
to EXPIRED evaluated on the kernel clock, idempotency keys the binding never dispatches
twice, and ack levels mapped from the asset protocol (edge received, asset received from the
cFS command accept, executed from the ADCS app's report). Transitions are events in the run
stream; the replay binding reproduces the trail bit for bit. Tests: the demo attitude DRM
with a mode command driven through the service; a deadline that expires undispatched; a
duplicate idempotency key; the replay of a run with commands equal to the live run.

**A4 The AI plane.** `crates/av-gateway`: a read-only, label-aware gateway over run
products (trajectories, events, scores, measurements by run hash) exposed as gRPC and as an
MCP server with a deny-by-default tool allow-list, and a `propose_command` tool that can
create PROPOSED and nothing else, attributing the proposal to the model identity and
version and recording it in an evidence topic on the ledger (what the model saw: the run
hash and the query ids). `crates/av-proposer`: the first model sidecar, a deterministic
rule-based proposer (a station-keeping burn when the scored radius drifts past a
threshold) implementing spoore's `ModelService` contract, run as a labelled container with
`--network none` plus the gateway endpoint; docker-gated tests skip visibly. Tests: the
proposer cannot reach any other state (a crafted call to authorize is refused and counted);
a proposal carries the evidence ids that replay resolves; the gateway refuses a query above
the caller's label; the same run always yields the same proposal.

**A5 The console.** A command console panel in the tiling layout, present in the execution
profile's default layout only: proposals with their rationale and evidence, the policy
decision, an authorize action that sends the operator's token, the trail per command, the
counters. Server routes under `/api/command/`. Headless checks against a real service run;
the lead drives it in a browser at acceptance.

**A6 Replay and compliance.** A replayed run reproduces every transition, decision id and
proposal from the ledger; a control matrix for `av-command` and `av-gateway` in the
secrouter format under `docs/compliance/`; the evidence bundle from both services collects
in one call.

## Exit

The proposer, running isolated, proposes a burn from a scored run through the gateway; the
policy checks it; a human with the right role and MFA authorizes it from the console; the
simulated asset executes it through the real command path and acks at three levels; a
deadline and a wrong role are refused with their reasons; the replay reproduces the trail
including what the model saw; the ledger verifies; evidence and control matrices exist.
Every number in the status section is traceable to a run hash.

## Status (AI-plane manager, 2026-09-12)

**Round 1 delivered A1 and A2. A3 was not started** — five tasks filled the round, and A3
(the kernel telecommand binding, deadlines on the kernel clock, ack levels from the cFS
command accept, and a replay equal to the live run) is a full task on its own that would
have been rushed. Five commits on `aiplane`, one per accepted task:

| Commit | Task |
|---|---|
| `4a7fb78` | A1.1 the command authority library, ledger and `authority.proto` |
| `92b24ac` | A1.2 Rego policy at CHECKED, and the decision in the ledger |
| `9f21548` | A1.3 `CommandAuthorityService` over the OpenSSL-backed tonic stack |
| `55b06c4` | A2.1 principals from OIDC tokens, verified against the system OpenSSL |
| `86aa136` | A2.2 role gating, MFA, delegations and the audit line |

### Gates, the manager's own runs at `86aa136` with no worker active

| Gate | Result |
|---|---|
| `cargo test -p av-command` | **131 passed**, 0 failed, 0 ignored (107 lib, 21 `grpc_service`, 3 `policy_fixture`) |
| `cargo test --workspace --exclude av-kernel --no-fail-fast` | **417 passed**, 0 failed, 0 ignored (286 was `develop`'s baseline; the 131 are this crate) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean, 0 warnings; `grep -rn "#\[allow" crates/av-command/` is zero hits |
| `cargo deny check` | `advisories ok, bans ok, licenses ok, sources ok` |
| `.venv/bin/python -m pytest -q -rs` | **498 passed, 3 skipped**, every skip printing its reason |
| `buf breaking` | **not run, and not required**: `git diff --name-status develop...HEAD -- proto/` shows one entry, `A proto/altavista/v1/authority.proto`. No shared proto file was touched. `buf` is also not installed on this host, which the lead should know before a round does need it. |

Full gate output is in the round's scratchpad.

### The regorus gate (question 201(a)), settled at setup

The plan asked for regorus with its crypto built-ins disabled by feature. **regorus 0.12.0
has no crypto built-ins at all** — no `crypto` feature exists and there is no
`src/builtins/crypto.rs` — so there was nothing to disable. The real hazard was elsewhere:
its `std` feature declares `rand/std` without the optional-dependency `?`, which forces the
`rand` crate in and with it `chacha20`. The crate is therefore pinned at
`default-features = false, features = ["arc", "regex"]`, which also leaves the `http` and
`net` features off, so the evaluator has no network builtin — a question 154 guarantee as
well as a crypto one. `cargo tree -p av-command` shows no `ring`, `sha2`, `md-5`, `sha1`,
`blake2`, `chacha20`, `rand` or rustls.

`rand_chacha` does appear in this crate's tree through `tonic → tower → rand`. It is
already in `develop`'s `Cargo.lock` and identical in `av-grpc`'s and
`av-dynamics-service`'s trees since M5.3, `deny.toml` does not ban it, and this track
introduces nothing new; recorded rather than changed. `sha2` likewise stays where it was,
in `av-dynamics`, never in `av-command`.

### What A1 and A2 actually are now

The state machine is a library with one function per edge and a typed refusal for every
illegal edge, pinned by a test over the whole 7×9 product: nine legal pairs, 54 typed
refusals. Propose-only (question 53) is enforced in code, not only in policy. The ledger is
file-backed, append-only and chained per entity with SHA-256 through the `openssl` crate
and the `GENESIS` convention from `envelope.proto`; `verify` walks a partition from disk and
names the sequence a tamper broke at; two runs over the same inputs give byte-identical
files, which holds because nothing in the crate reads a wall clock or generates a random id.

Policy is Rego, evaluated in process with a fresh engine per decision, over a canonical JSON
input that is also the decision-id preimage. The calling convention is `allow ∧ ¬deny`:
both entry points are evaluated every time and a non-empty deny set overrides a true allow.
Evaluation failures are never swallowed — a rule that errors, returns the wrong shape, or
yields a non-string set element each becomes a named reason. The shipped
`profiles/policies/authority/command.rego` admits `mode`, rejects `payload`, rate-limits
`burn`, and refuses any non-empty `envelope_id` for every class. A CHECKED or REJECTED
record carries the decision and the input it was made over, so both re-evaluate from disk
to the same decision id and policy hash.

The service is `CommandAuthorityService` over tonic with `tls` never enabled, plaintext on
loopback only and a typed refusal of any non-loopback bind naming question 155. Dispatch
never sends an idempotency key twice and that guarantee survives a restart, rebuilt from
the ledger's DISPATCHED records before the first RPC is served. Nine integration tests
drive a real service over a real loopback socket.

Identity is real: a compact JWS verified with `openssl::sign::Verifier` alone, RS256, with
sixteen individually typed refusals including `alg: "none"`, and expiry evaluated against
the injected clock at the boundary nanosecond. Authorization is role-gated per class from
the profile's table, deny by default, with MFA required for a hazardous command and
time-limited delegations whose expiry uses the same boundary convention as the token's.
Every outcome, refusals included, writes one RFC 5424 line whose timestamp is the injected
TAI epoch converted to UTC, so two identical runs produce identical lines.

### Declared gaps, all deliberate

- **ES256/ES384** are not implemented; RS256 only. A JWS ECDSA signature is raw `r||s` and
  needs a DER conversion that is not a few clear lines, so it is a named gap rather than a
  half-implementation.
- **The OPA cross-check is a pinned document shape, not an executed comparison.** No OPA
  binary exists here and fetching one would be network at test time (question 154). The test
  says so in its own doc comment; nothing claims OPA compatibility is verified.
- **The audit sink is a file, not a SIEM.** Question 54's SIEM export is the format, written
  where a forwarder can read it; no UDP or socket target was added because it would be
  untested here. The control matrix says so rather than claiming the control.
- **`acr` is an exact string match**, with no ordering or hierarchy; `amr` containment is
  the primary MFA check.

### Defects found in review, with their root causes

Five, and every one was the same shape — **a failure or refusal that left no visible trace**:

1. **`#[allow(clippy::too_many_arguments)]` in `ledger.rs`**, in a task that reported "no new
   `#[allow]`". Cause: the claim was made without grepping the file. Fixed by giving the
   hash body its own type rather than suppressing the lint; the on-disk bytes are unchanged.
2. **The ledger's partition filenames were not injective.** The escape alphabet was not
   prefix-free — `_` was both the escape character and a passthrough character, so `"a/b"`
   and `"a_002f_b"` mapped to one file and two entities' chains would have interleaved
   silently. Filenames are now the SHA-256 hex of the partition name, with injectivity and
   the directory-escape property asserted over an adversarial table.
3. **The policy evaluator short-circuited `deny` whenever `allow` was true** — a fail-open
   direction that no test could reach, because the shipped policy guards every allow rule.
   Overruled and fixed to `allow ∧ ¬deny`, pinned by a fixture the shipped policy cannot
   itself produce.
4. **The evaluator swallowed its own errors**, mapping every `Err` and wrong-shaped result
   to nothing. This is how the same task's own `sprintf("%q", …)` defect nearly shipped:
   regorus does not implement Go's `%q`, the deny reason vanished, and only an integration
   test noticed. Three named reasons now cover the three shapes.
5. **The idempotency guarantee evaporated on restart.** The dispatched-key set was built
   empty and never read the ledger, so a restarted service would re-dispatch a key it had
   already sent — the moment a duplicate is most likely, against a `command.proto` contract
   that is unconditional. `LedgerRecord` gained `idempotency_key` and the set is rebuilt
   before the first RPC; the test builds a second service over the same directory.

A sixth, caught before it mattered: the test issuer was an ungated `pub mod`, putting a
token-minting facility in the shipped library's API. Now behind a `test-support` feature,
proven absent from a default build by `cargo tree` over normal and build edges and by `nm`
finding no such symbol in the binary.

One defect was found by a worker rather than by review and is worth keeping: an early
`resolve_loopback_bind_address` would have panicked in production on a bare `"localhost"`
with no port, because `"localhost"` passes the loopback check and then `rsplit_once(':')`
returns `None`. A unit test caught it; a `MissingPort` variant fixed it.

### Open items for the lead

1. `buf` is not installed on this host. This round did not need it, but a round that touches
   a shared proto will.
2. `altavista/pb/generate.py` needs `grpcio-tools`, which the `dev` extra does not declare,
   so the documented regeneration command fails on a clean dev install. Not fixed here:
   `pyproject.toml` is shared with the edge track and a change would conflict.
3. `oidc.rs` detects "this rule is not defined at all" by matching regorus's own error text.
   It is documented as 0.12.0-specific with a note on what breaks if it changes, but it is a
   string match on a dependency's prose.
4. `rand_chacha` through `tonic → tower → rand` is pre-existing and lead-accepted since
   M5.3; recorded here so the crypto rule's "no bundled crypto" wording and that crate can be
   reconciled deliberately rather than re-litigated each round.

## Status (AI-plane manager, 2026-09-12) — round 2

**Round 2 delivered the two question-203 fixes, all of A3, and A4's gateway half.
`crates/av-proposer` (A4's sidecar) was not started** and is the one deliberate omission —
five tasks filled the round, and the proposer carries a design decision (below, decision 11)
that should be ratified before it is built rather than after. Five commits on `aiplane`, one
per accepted task:

| Commit | Task |
|---|---|
| `33929c9` | R2.1 question 203(a) full `Command` on `LedgerRecord`; 203(b) the regorus prose tripwire |
| `60921fa` | A3.1 the kernel's `ExternalCommandSource`, deadlines, idempotency, three real ack levels |
| `20461ec` | A3.2 the service→kernel adapter, the tenth legal edge, `Expire`/`Fail` |
| `6c112d9` | A3.3 a replayed run reproduces the command trail bit for bit |
| `36a0ca9` | A4a `crates/av-gateway`, the read-only label-aware gateway and its MCP surface |

### Gates, the manager's own runs at `36a0ca9` with no worker active

| Gate | Result |
|---|---|
| `cargo test -p av-command -p av-gateway` | **187 passed**, 0 failed, 0 ignored (av-command 138: 112 lib, 23 `grpc_service`, 3 `policy_fixture`; av-gateway 49: 42 lib, 4 `propose_only`, 3 `real_run_products`) |
| `cargo test --workspace --exclude av-kernel --no-fail-fast` | **553 passed**, 0 failed, 1 ignored (417 was round 1's figure; the rest is this round plus the `develop` merge) |
| `cargo test -p av-kernel` | **901 passed**, 0 failed, 1 ignored; the four cFS-image tests skip visibly, each naming the missing image and its build command (question 194) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean, **0 warnings**. `grep -rn "#\[allow" crates/av-gateway/` is zero hits; the diff adds no `#[allow]` anywhere this round |
| `cargo deny check` | `advisories ok, bans ok, licenses ok, sources ok`. `cargo tree -p av-gateway` has no `ring`, `sha2`, `rustls`, `chacha20`, `md-5` or `blake2` |
| `.venv/bin/python -m pytest -q -rs` | **505 passed, 3 skipped**, every skip printing its reason. This track added no Python test; the delta from round 1's 498 is the `develop` merge |
| `buf breaking` | **clean against this branch's own merge base `a7623e5`, exit 0, zero findings.** Against `/Users/probe/code/AltaVista/proto` it exits 100 with 15 findings, **every one of them in `edge.proto`** and none outside it — see decision 8 |

Full gate output is in the round's scratchpad. Only `proto/altavista/v1/authority.proto` (this
track's own file) changed this round, plus its regenerated Python bindings; no shared proto
was touched.

### The manager's decisions this round

1. **A command-authority service is not a `DynamicsModel`**, so it did not become a
   `ModelKind`/`BindingPlan` variant — that would have been a lie about what the type is.
   There is also no dynamic binding registry in this codebase: `BindingPlan` is a closed enum,
   `classify_binding` its constructor, `registry::kind_for` a string-prefix dispatcher. The
   binding-registry change A3 genuinely needed is the **attitude controller gaining a FRAMED IN
   command port**, resolved by `classify_binding` the way that binding kind already resolves its
   star, IMU and wheel-torque codecs. The service enters the run through `RunConfig.
   command_source`, shaped exactly like the existing `replay` field.
2. **`poll` is called once, at the scenario start epoch.** `run_shared_group` is a batch
   simulation with no wall clock to wait on between two simulated instants, so a command's
   whole `not_before`/`deadline` fate is decidable analytically from three numbers by one pure
   function. The consequence is stated in the module doc rather than left to be discovered: **a
   command the service authorizes after a run has started cannot reach that run.**
3. **The cFS command accept and the ADCS execution report are a declared gap**, verified not
   assumed: `adcs_app.c` subscribes only to the star-tracker, IMU and wakeup MIDs, and
   `services/cfs/` has no command-accept counter anywhere. Adding a ground-telecommand path
   there is a SIL-track change needing an image rebuild, on a host that prunes that image
   (question 196(d)). The three ack levels are delivered instead from the SIL asset's own real
   wire packets, and `ACK_LEVEL_ASSET_RECEIVED` is now a **real decode ack** rather than the
   inference from an `AppliedCommand` that it was before.
4. **The levelled ack is opt-in by ack-codec shape**, so no existing fixture's port traffic or
   `port_traffic_hash` moves. No golden moved this round.
5. **A tenth legal edge, `ACKED -> ACKED`, permitted only on a strictly increasing ack level.**
   `CommandTransition.ack_level` exists so an asset acking at three levels produces three
   transitions; if a later ack could only be dropped once `state` was `ACKED`, that field would
   be pointless and no ledger, replay or console could ever see the asset's more-executed acks.
   A non-increasing level is the new typed refusal `AckLevelNotIncreasing`, kept distinct from
   `IllegalTransition` because the edge does exist and it is the *value* being refused. The
   product test is now **ten legal pairs and 53 typed refusals** over the same 7×9.
6. **`Expire` and `Fail` as additive RPCs**, so every kernel refusal a command survived
   `Dispatch` for lands on the ledger. Without them a command the kernel refused would sit at
   DISPATCHED forever with no record of what happened to it.
7. **A3's wiring lives in `crates/av-run`, not `av-command`**, so `av-command` stays free of an
   `av-kernel` dependency: `av-kernel` pulls GMAT in through `gmat-sys`, and
   `cargo test -p av-command` must not need GMAT. No new crate and no cargo feature.
8. **`buf breaking` is run against this branch's own merge base**, not the main tree. The edge
   track's round 2 landed a larger `edge.proto` in the main tree mid-round (201 lines here, 381
   there), so `--against /Users/probe/code/AltaVista/proto` reports that track's newer content
   as deletions from this branch. Both runs are recorded above; the merge-base run is the one
   that isolates this track's own change.
9. **"An evidence topic" is a ledger partition, not a broker.** The phrase appears only in
   `architecture.md`; nothing in code implements a topic and there is no Kafka or Redpanda
   crate in the tree. The proposal evidence record is an additive message on `authority.proto`
   written to the existing ledger, with the module doc saying a broker can replace it later —
   the same call the edge track made for its durable log (question 200(d)).
10. **The MCP server is hand-rolled JSON-RPC 2.0 over stdio**, with no new crate. No MCP or
    JSON-RPC implementation exists in this workspace or in spoore's, `serde_json` is already a
    workspace dependency, and `admin.rs` is the precedent for a hand-parsed minimal server.
    stdio also means no listening socket, which is question 154's guarantee for free.
11. **For `av-proposer`, not yet built, two decisions the lead should ratify before it is:**
    (a) "`--network none` plus the gateway endpoint" is only coherent if the endpoint is a
    **bind-mounted Unix domain socket** — with `--network none` there is no TCP path at all, so
    a UDS is both the only possibility and strictly stronger than an internal Docker network
    (no DNS, no other container reachable, no egress), and it makes the isolation assertable.
    (b) The `ModelService` contract comes from spoore **by path dependency**, not by vendoring
    its proto: `spoore-cdm` is already a workspace path dependency (question 12's precedent),
    and `av-proposer`'s `build.rs` can mirror `spoore-ml/build.rs` with `build_server(true)`,
    since spoore deliberately generates no Rust server of its own.

### What A3 and A4a actually are now

`ExternalCommandSource` is a two-method trait — `poll` and `report` — whose commands run
through the existing M25.2b machinery unchanged: `command_out_packet_codec`,
`assign_sequence_numbers`, `dispatched_event`, `acked_event`, `ack_emission_epoch` and
`router.deliver` are reused, never duplicated into a second path. `CommandOutcome` is
exhaustive over every way a command can fail to reach ACKED — expired, run ended before
`not_before`, duplicate idempotency key, and four structural refusals — and every one is
reported, so no refusal leaves the run without a trace. The deadline's boundary convention is
`av-command`'s own (`now >= expiry` refused, one nanosecond earlier not), tested on both sides;
`deadline_tai_ns == 0` is no deadline. Idempotency is enforced kernel-side as well as in the
service, because `command.proto`'s contract is unconditional, and an empty key is deliberately
never a duplicate of another empty key.

The attitude controller's one writable parameter is `mode`: REGULATE runs the PD law, SAFE
publishes an explicit zero wheel torque rather than publishing nothing, so a mode change is
visible on the wire and in the trajectory rather than being an absence. An out-of-allowlist
mode value is a recorded decode-error episode with the last good mode retained (question 188's
shape), never a silent ignore and never a run abort. The end-to-end test measures the physical
effect: SAFE leaves `pointing_error_rad` at 0.0858 against the always-REGULATE 0.0816.

Replay now reproduces the trail. The executor derived ACKED only from `span.applied_commands`,
and a replayed instance has none — `ReplayModel` correctly computes no `AppliedCommand`, and one
that fabricated one would be lying about what it did, which is why `tests/replay.rs`'s fixture
used to avoid a command target. With the levelled ack on the wire the evidence is in the
recorded port traffic, so a replayed target's ACKED is derived from that recorded frame,
correlated by the packet's own `cmd_seq` through the existing sequence map. `Router::
take_port_traffic` is a genuine drain, so the one drain moved into `run_shared_group` and is
threaded back out rather than letting `execute()` drain it a second time and get nothing.
Question 187's epoch trap, on its third encounter, is handled by inverting the relation through
`ack_emission_epoch` with a negated period rather than an open-coded subtraction, and the test
asserts the replayed ACKED lands on the live run's exact `tai_ns`. The byte-identity assertion
has a separate non-vacuity block beside it, because two runs with no command events would
compare identical while proving nothing.

`av-gateway` routes every refusal through one `Counters` primitive rather than ad hoc
per-module counters, so ADR-004's "everything rejected is counted" is mechanical rather than
something each module has to remember. Addressing is identity-never-a-path, copied from
`altavista/server.py`'s sweep-sample handler including its ordered typed-refusal chain; labels
are `av-edge`'s clearance ladder verbatim, with a marking absent from the ladder refused on
either side of the comparison rather than defaulted to a rank. `propose_command` is
structurally propose-only: `ProposeOnlyAuthority` exposes only `connect`, `from_channel` and
`propose`, and the generated client carrying `check`/`authorize`/`dispatch`/`ack`/`expire`/
`fail` is private to it. The crafted-call tests come from several directions — a `tools/call`
naming `authorize`, a raw JSON-RPC method named `authorize`, and `tools/call` naming each of the
other five — and each asserts the refusal *and* the counter. `tools/list` is derived from the
same table `dispatch` uses, with a test that the two can never disagree.

### Declared gaps, all deliberate

- **`crates/av-proposer` does not exist.** A4's gateway half is done; the sidecar is not.
  Decision 11 above is why it was not rushed.
- **A command authorized after a run has started cannot reach that run** (decision 2).
- **The cFS command accept and the ADCS execution report** (decision 3).
- **`ExternalCommandSource` against a GMAT-bound target is wired but has no dedicated
  acceptance test** — it reuses the identical resolution the DRM-declared path already tests,
  and `ConstantAccel` and `Controller` targets are both exercised.
- **The gateway's run-products fixture carries no command events.** It is a real, tracked
  `RunProducts` from a real `execute()` run (`tests/fixtures/demo_measurements.runproducts.bin`,
  committed at `a25dd05`), which is what "not a hand-built literal" required, but it is the
  measurements demo — so the gateway is not yet proven against a run whose events include a
  command trail.
- **`ExpireRequest`/`FailRequest` carry no principal**, attributing to a fixed service
  principal, while `AckRequest` carries an unverified caller-supplied one. Neither is
  authenticated, which is consistent with question 155's loopback-plus-nginx-mTLS posture that
  A1.3 was accepted under, but the asymmetry is now in the wire contract.
- **ES256/ES384** are still not implemented; RS256 only, as question 203(c) ratified.
- **The OPA cross-check** is still a pinned document shape, not an executed comparison.
- **The audit sink** is still an RFC 5424 file, "Partial" for the SIEM control per 203(d).

### Defects found in review, with their root causes

1. **The `LedgerRecord.command` self-evidence invariant was documented but unenforced.** The
   field's doc comment states that the attached `Command` is the post-transition value, and the
   task pinned it with a fixture that built a consistent `Command` and checked it had stayed
   consistent — a tautology. Nothing stopped a future call site attaching a stale `Command`, and
   the ledger would have recorded the disagreement durably for `scan_commands` to hand straight
   back to `Query`. Root cause: the invariant lived in prose and in one hand-built fixture,
   never in code. Fixed by checking it in `Ledger::append`, which **immediately caught the same
   task's own `scan_commands` fixture violating it** on the first run.
2. **A3.1 broke `cargo clippy --workspace --all-targets`.** Adding `RunConfig.command_source`
   left the `RunConfig` literal in `crates/av-sweep/src/bin/av-sweep/sample_mode.rs`
   unupdated, so the workspace lint failed from `60921fa` until `20461ec`. Root cause: both the
   task's own gate and **the manager's review** ran clippy per-crate (`-p av-kernel -p av-run`),
   not workspace-wide, and a new required struct field is exactly the change only the
   workspace-wide run catches. Found by the next task. Standing lesson for this track: a task
   that adds a field to a type other crates construct must run the workspace clippy, not a
   per-crate one.
3. **A worker spent its whole turn waiting and produced nothing.** A3.3's first attempt ran a
   baseline suite in the background, ended its turn on the wait, and left an empty diff — 128
   tool uses for no artifact. Root cause: the brief permitted background execution and did not
   supply the baseline. Fixed by resuming the same agent with the baseline figures supplied and
   background waits forbidden; it then delivered the whole task. Briefs on this track now say
   "no Monitor, no `run_in_background`, wait in the foreground."

Round 1's lesson held: every defect this round was again **a failure that would have left no
trace** — an unenforced invariant, a lint that only one gate shape catches, and a worker whose
silence was indistinguishable from progress.

### Open items for the lead

1. **Decision 11 needs ratifying before `av-proposer` is built** — the Unix-socket reading of
   `--network none`, and the path dependency onto spoore's `model_service.proto` rather than
   vendoring it.
2. **`buf breaking` against the main tree is no longer a usable gate mid-round** while two
   tracks own different proto files and the main tree moves under both. Either the gate compares
   against each branch's own merge base, or the lead runs it once at the acceptance merge.
3. **The non-`Authorize` RPCs are unauthenticated** (`Dispatch`, `Ack`, and now `Expire`/`Fail`),
   and `Expire`/`Fail` do not even accept a caller-supplied principal while `Ack` does. Worth a
   deliberate decision on whether any of them ever needs a verified principal, rather than
   letting the asymmetry settle by accident.
4. **`docs/aiplane-plan.md`'s round-1 open item 3 names the wrong file** — the regorus prose
   match is in `policy.rs`, not `oidc.rs`. Left as written so the record is not rewritten after
   the fact; corrected here.
5. **`altavista/pb/generate.py` and `grpcio-tools`**: round 1's open item 2 is closed by the
   edge track's `pyproject.toml` fix, which this branch took at the merge (question 202). The
   regeneration command worked this round.
6. **The cFS image was absent for this whole round** (question 196(d), sixth disappearance by
   the standing count). Nothing this track needed it for — the demo attitude command fixture is
   native — and the four gated kernel tests skipped visibly throughout.
