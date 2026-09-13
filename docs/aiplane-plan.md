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

## Status (AI-plane manager, 2026-09-13) — round 3

**Round 3 delivered the service principals, all of A4b (the proposer, its container and the
egress proof), question 206's two remaining open items, all of A5 (the console), and A6.**
Seven commits on `aiplane`, one per accepted task:

| Commit | Task |
|---|---|
| `f07300f` | R3.1 service principals on `Dispatch`, `Ack`, `Expire` and `Fail` |
| `1f87e9f` | A4b `crates/av-proposer`, the deterministic rule-based proposer |
| `4a545b1` | A4b the proposer's container, its internal network and the egress proof |
| `c473385` | Question 206's two open items: the gateway on a command trail, a GMAT-bound `ExternalCommandSource` target |
| `ac335ce` | A5 persist the proposal, and serve the console's data under `/api/command/` |
| `3936f0e` | A5 the command console panel, in the execution profile's default layout only |
| `19c77a4` | A6 the ledger decision trail replayed, the control matrices, one evidence bundle |

### Gates, the manager's own runs with no worker active

| Gate | Result |
|---|---|
| `cargo test -p av-command -p av-gateway -p av-proposer` | **281 passed**, 0 failed, 0 ignored (av-command 178 = 143 lib + 32 `grpc_service` + 3 `policy_fixture`; av-gateway 77; av-proposer 26). 187 was round 2's figure |
| `cargo test --workspace --exclude av-kernel --no-fail-fast` | **733 passed**, 0 failed, **3 ignored** (639 and 2 ignored was this round's measured baseline; the third ignore is this round's own new fixture generator) |
| `cargo test -p av-kernel` | **902 passed**, 0 failed, 2 ignored; the four cFS-image tests skip visibly, each naming the missing image and its build command (question 194) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean, **0 warnings**. `grep -rn "#\[allow" crates/av-command/ crates/av-gateway/ crates/av-proposer/` is **zero hits** (the generated `pb` modules carry the established inner `#![allow(clippy::all)]`, as every other crate's generated module in this workspace does) |
| `cargo deny check` | `advisories ok, bans ok, licenses ok, sources ok`. `cargo tree -p av-proposer` has no `ring`, `sha2`, `rustls`, `chacha20`, `md-5` or `blake2` |
| `.venv/bin/python -m pytest -q -rs` | **539 passed, 5 skipped** (513 and 4 was this round's measured baseline), every skip printing its reason — the four pre-existing image gates plus the proposer's own container test |
| `buf breaking proto --against <merge base 8184764>` | **exit 0, zero findings.** `git diff --name-status` over `proto/` shows one entry, `M proto/altavista/v1/authority.proto` — this track's own file, additive only. No shared proto was touched |

Full gate output is in the round's scratchpad.

### The manager's decisions this round

1. **The counting primitive moved to `av-command`.** `av-command` had no counter surface at
   all, while `av-gateway` had the right one (`Counted`/`Counters`, `BTreeMap`-backed,
   deterministic snapshot). `av-gateway` depends on `av-command` and the reverse is
   impossible, so the module moved down and `av-gateway` re-exports it. One primitive, no
   duplicate, and ADR-004's "everything rejected is counted" is now mechanical in both crates.
2. **A service principal is structural, not a claim.** A verified token is a service principal
   exactly when one of its groups grants, through the profile's new `authority.service_roles`
   table, the specific RPC being called — and the service-role keys are required **disjoint**
   from the human role keys at config load, with a typed error naming the overlap. Without
   that disjointness a human authorizer's group would silently become a dispatch credential
   and a token minted for a person could drive the asset. A purely human token is refused on
   all four RPCs by the same deny-by-default path.
3. **A caller-declared `principal` that disagrees with the verified subject is refused**
   (`INVALID_ARGUMENT`), never silently preferred in either direction. An empty declared value
   declares nothing and is never a disagreement. This closes round 2's declared asymmetry
   (`Expire`/`Fail` carried no principal while `Ack` carried an unverified one) without
   inventing a precedence rule nobody could predict.
4. **`DISPATCH_PRINCIPAL`, `EXPIRE_PRINCIPAL` and `FAIL_PRINCIPAL` are deleted.** A fixed
   string principal means nothing once identity is real, and a constant that means nothing is
   worse than no constant.
5. **A new additive `ModelProposeService` on `authority.proto`, served by `av-gateway`.**
   `propose_command` existed only as an MCP tool over stdio, and `DataGatewayService`'s own
   contract promises it can create nothing — so a containerised proposer talking to the
   gateway over a network had no propose path at all. Adding a propose RPC to
   `DataGatewayService` would have broken its stated read-only guarantee, and MCP-over-TCP
   would have invented a transport. A second, structurally separate service puts on the wire
   the same split `ProposeOnlyAuthority` already makes in code. The MCP tool and the RPC share
   **one** implementation (`crates/av-gateway/src/propose_flow.rs`) with a test that the two
   surfaces agree on outcome and on refusal: two independently maintained propose paths is how
   a propose-only guarantee gets lost on one of them.
6. **`av-proposer` generates no `CommandAuthorityService` client at all.** It never parses
   `authority.proto`; its two clients come from hand-declared `tonic_build::manual` descriptors
   over the already-compiled `av_cdm::pb` types, so propose-only is a property of what exists
   rather than of a wrapper's discipline. A test greps this crate's own `OUT_DIR` for the
   absence. The cost is that the two method paths are hand-written rather than generated; the
   integration tests drive a real server over them, which is what catches a divergence.
7. **spoore's `ModelService` is served for real, and `ModelInfo` is the load-bearing half.**
   `WarmStart`/`Predict`/`MeasurementLikelihood`/`Update`/`ClaimTestability` delegate to a
   `spoore_models::KalmanFilter` over `ConstantVelocity3d`, built exactly as
   `crates/av-track/src/config.rs` already builds one — a real deterministic closed-form
   filter, `epistemic_method = "exact"`, not a stub answering `UNIMPLEMENTED`. `ModelInfo`'s
   `node_id` and `version` are the same two values that land in `ProposalEvidence`, asserted by
   a test, so the sidecar's declared identity and the attribution on its proposals cannot
   drift apart.
8. **Decision 11(a)'s internal network needs the gateway to leave loopback, and that is an
   explicit opt-in, not a default.** `resolve_internal_network_bind_address` accepts a loopback
   spelling (by delegating to the existing resolver), the unspecified address and RFC-1918
   literals, and refuses any global-scope address with a new typed, counted refusal. It is
   reached only through `av-gateway`'s `--internal-network-bind` flag, never an environment
   default; question 155's plaintext-on-loopback rule and its typed refusal are untouched
   everywhere else, and across hosts the answer is still the nginx mTLS front.
   **The rejected alternative and why:** putting the proposer in the gateway container's
   network namespace (the edge plugin's own trick) would also expose the **command
   authority's** loopback port to the proposer, and the isolation claim — "the proposer can
   reach the gateway and nothing else" — would then be false. The authority instead runs on
   the gateway container's own loopback, in a namespace the proposer is not in.
9. **The console's profile signal is stamped the way the imagery already is.** The browser had
   no profile identity at all; `Hub` now learns the active profile id from `create_app(profile=)`
   and stamps `scenario.profileId` beside `scenario.imagery`, only when absent, so every
   scenario published before this change behaves exactly as it did. "Execution profile only"
   (question 201(d)) governs the **default layout**; the panel joins the registry so any pane
   can be swapped to it in every profile, which is availability, not default.
10. **`int64` leaves Python as a decimal string**, which is also protobuf's own canonical JSON
    mapping. See the defects below for what was actually happening before.
11. **The evidence bundle is an `av-gateway` admin route, not a new service.** The dependency
    direction only lets the higher crate see both sides. A section it could not collect is a
    named `{"reachable": false, "error": ...}` entry, never an omitted key — a bundle with a
    silently missing section is not evidence.
### What this round actually is now

`Dispatch`, `Ack`, `Expire` and `Fail` authenticate an OIDC service subject through the
identical `oidc::verify` path a human token takes — same issuer config, RS256 only, boundary-
nanosecond expiry on the injected clock, no second verifier and none of the sixteen typed
refusals relaxed. A service role table in `profiles/execution.yaml` grants a named subset of
the four RPCs, deny by default, disjoint from the human role table by a check at config load.
Every refusal on the five authenticated RPCs is typed, counted through the shared primitive,
reported by `/admin/api/evidence`, and written as one RFC 5424 audit line. `av-run`'s adapter
presents its service token on every `Ack`/`Expire`/`Fail`.

`crates/av-proposer` is a deterministic rule-based proposer: it reads a run's scores through
the gateway, and when the named radius score drifts past a declared threshold it proposes a
burn whose magnitude is a declared, clamped proportional law. It reads no wall clock, generates
no random value and iterates no `HashMap`; the command id and the idempotency key are two
SHA-256 digests over one canonical preimage distinguished by a purpose tag, through the
`openssl` crate, following `query_id.rs`'s convention. Two independent runs over the real
committed `demo_two_instance` fixture produce byte-identical `Command`s, the same evidence and
the same query ids, with a non-vacuity guard beside the byte-identity assertion. A score that
is absent, in the wrong unit, or non-finite is a typed, counted refusal — never defaulted to
zero and never compared across units.

The console is real on both sides. `/api/command/` serves the proposals with their rationale and
evidence, the policy decision with its id and policy hash, the trail per command, the counters
proxied from the real admin endpoint, and an authorize action that forwards the operator's token
verbatim and never stores, caches or logs it. The endpoints are configuration, never a request
parameter — the same "identity is never a path" rule `POST /api/cdm/sweep/sample` already
follows. With `grpcio` absent or no endpoint configured, every command route answers a typed 503
and every pre-existing route still works. The panel is framework-free, never fetches (every call
lives in `app.js`, the convention `openFeasibilitySample` set), shows a refusal with the
server's own reason, and offers no dispatch, ack, expire or fail control — propose-only stands.

A6 is done in all three parts. A replayed decision trail reproduces every transition, the
decision id and policy hash, and the proposal's rationale and evidence ids from the ledger
alone, from a second service over the same directory with the writing process gone; the
**decision id re-derives** through the public `policy::evaluate` over the recorded input and an
independently reloaded bundle, not merely matching a string read twice; a deliberate on-disk
tamper is caught and names seq 3 with the two earlier records still good; an empty-ledger
control proves the assertions discriminate. The control matrices for `av-command` and
`av-gateway` are in `docs/compliance/`, and both say plainly what is not met — including that
**`av-gateway` authenticates no caller of its own on any surface**. The evidence bundle is one
`av-gateway` admin call collecting both services, `BTreeMap` end to end, byte-identical across
two identical deployments, with an unreachable side named rather than omitted, and asserted to
contain no key, token or credential.

### Declared gaps, all deliberate

- **The proposer's container has never run end to end on this host.** The image cannot be built
  (see the host measurement below). `tests/test_proposer_container.py` is complete — the internal
  network, both egress proofs, the authority-unreachable measurement, the label cleanup guard —
  and it **skips visibly**, naming the missing image and `services/proposer/build-image.sh`. The
  isolation claim is therefore implemented and unexercised, and both control matrices say so.
- **`av-gateway` authenticates no caller.** `caller_clearance` and the proposing `principal` are
  caller-supplied strings; the MCP stdio surface trusts whoever holds the pipe. Consistent with
  question 155's loopback-plus-nginx posture, but it is now written down as a gap rather than
  left to be assumed.
- **`Propose` and `Check` still carry no credential**, by design (`authority.proto`'s own doc:
  a proposer is not a human OIDC principal), and `Query`/`VerifyLedger` remain reachable by any
  caller that can reach the bind address.
- **No token refresh in `av-run`'s adapter.** A run that outlives one token's lifetime would see
  its `Ack`/`Expire`/`Fail` refused; the module doc names the shape a solution would take.
- **`ES256`/`ES384`** are still unimplemented; RS256 only (question 203(c)). **The OPA
  cross-check** is still a pinned document shape. **The audit sink** is still an RFC 5424 file.
  **`acr`** is still an exact string match.
- **The panel does not poll.** Proposals and counters refresh on selection, on authorize, and on
  an execution-profile scenario load; a new proposal does not arrive on its own.
- **`av-gateway`'s binary configures its bind addresses from environment defaults** while
  `av-command`'s are CLI arguments only. The two control matrices disagree on this point
  deliberately rather than the newer one claiming a parity it does not have.

### The host, measured this round (question 205, and it now blocks more than the cFS image)

The Colima VM's container filesystem is at 99% with about 900 MB free, 870 volumes and 45 GB
reclaimable, owned by another workload. Measured in sequence:

- `debian:bookworm-slim` with 909 MB free: `apt-get update` succeeds, 9273 kB fetched.
- `docker pull rust:1.90-bookworm`: available goes from 909 MB to **0**, 100% full.
- the same `apt-get update` inside that image at 0 bytes free: *"At least one invalid signature
  was encountered"* on all three `InRelease` files.

The signature error is apt's symptom for a truncated download, not a key problem — which is why
a direct `gpgv` on a freshly fetched `InRelease` reports a good signature. **No image that needs
the Rust toolchain image can be built on this host** until those volumes are reclaimed. The
pulled toolchain image was removed again so the host was not left at 0 bytes for the other
tracks. This is the same root cause question 205 already named for the cFS image's
disappearances, now shown to block image *builds* and not only to garbage-collect them.

Also measured, and worth the lead's attention: **`regorus 0.12.0` does not compile on Rust
1.85**, the workspace's declared `rust-version`. It needs `const_vec_string_slice`. The host
toolchain is far newer so nothing noticed, but the declared MSRV has been false since round 1
and any container build or CI pinned at 1.85 will fail.
### Defects found in review, with their root causes

Nine, and the shape held for the third round running — **a failure, refusal or guarantee that
leaves no trace**:

1. **The ledger's self-evidence invariants were `debug_assert_eq!`.** Round 2's review installed
   the `LedgerRecord.command` invariant "in `Ledger::append`", and round 2's status and the
   lead's acceptance both record it that way. `debug_assert!` is compiled out of a release
   build, so the guarantee held in the test binaries and **vanished in the shipped `av-command`
   binary and in the container image** — exactly where a durable, self-contradictory ledger
   record would matter. Root cause: the fix was written with a macro whose name reads like an
   assertion and whose behaviour is conditional on the build profile, and nothing in the
   review checked which profile it survives. Both invariants (and this round's new `proposal`
   one) are now real checks in every profile that **refuse the append** rather than panicking a
   running service: the record is never written and the refusal is counted. The tests now assert
   a returned `Err` *and* that nothing was written, which a panicking check could never
   establish.
2. **`Propose` discarded the rationale and the evidence ids.** `CommandProposal.rationale` and
   `.evidence_ids` were accepted on the wire, used for nothing, and unrecoverable afterwards —
   so A5's "proposals with their rationale and evidence" could not have been served at all.
   Root cause: `propose` destructured the proposal for its `command` and dropped the rest, and
   no test ever asked for them back. `LedgerRecord.proposal` now carries them, enforced
   self-evident, rebuilt from disk by `scan_proposals`, and returned by `Query`.
3. **A TAI nanosecond epoch was serialised to the browser as a JSON number.** These are about
   1.79e18, two orders of magnitude past JavaScript's `MAX_SAFE_INTEGER`, so `JSON.parse`
   rounded every transition epoch silently. Measured: `1789296161430000000` becomes
   `1789296161430000128`. Worth recording because the **first version of the check missed it**:
   comparing `String(Number(x))` with `x` reports "no loss" at this magnitude, because the
   double's shortest round-trip decimal still prints the original digits once a
   microsecond-granularity epoch's trailing zeros hide the 256 ns spacing. The check compares
   `BigInt(Number(x))` with `BigInt(x)` instead. The rounding is invisible in exactly the way
   that makes it dangerous.
4. **All four service RPCs loaded the command before authenticating**, so a caller with no
   credential at all could tell `NOT_FOUND` from `UNAUTHENTICATED` and enumerate the command
   ids the service holds. The task's own recorded reasoning claimed the opposite property.
   Authentication now precedes the lookup, pinned by a test that drives all four RPCs with no
   token against both an existing and a missing id.
5. **A service-principal refusal was counted but wrote no audit line**, while every `Authorize`
   refusal has written one since A2.2. A counter lives only in this process and behind the
   admin endpoint, so a refused dispatch was invisible to the sink question 54's SIEM export
   reads. `refuse_service_call` now writes one RFC 5424 line per identity or authorization
   refusal; `entity` and `class` are left empty because the command genuinely has not been
   loaded at that point, and the writer omits an empty parameter rather than inventing one.
6. **`Query`'s and `VerifyLedger`'s refusals never reached the counters** — raw `Status`
   constructions, so the refusals a malformed caller produces most often were the ones
   ADR-004's rule missed. Found by a worker and left; fixed with a typed `MalformedRequest`
   variant covering `Propose`'s four as well.
7. **An `EvidenceRecorder::record` I/O failure bypassed the counting path** in the MCP
   `propose_command` tool — a real evidence-ledger write failure (full disk, permissions) was
   reported to the caller and never counted. Found by a worker while extracting the shared
   propose flow, and fixed there so both surfaces record it identically.
8. **`av-gateway` raced its gRPC server against its MCP stdio loop in one `tokio::select!`.**
   The MCP loop ends the instant stdin reports EOF, and a container started with `docker run -d`
   and no `-i` gets EOF immediately — so the gRPC port closed a fraction of a second after
   opening, `docker run -d` exited 0, and the container's own logs said nothing. Found as a
   workaround by a worker (`-i` on its container) and left in the binary; fixed: the MCP surface
   ending at EOF is reported as the end of that surface alone, and only the gRPC server's exit
   ends the process.
9. **The two services' default addresses did not agree with each other.** `av-gateway`'s default
   gRPC bind was `127.0.0.1:50170`, which is `av-command`'s `DEFAULT_ADMIN_BIND` — two services
   of this same track started with nothing but their defaults fought over one port. And
   `AV_GATEWAY_COMMAND_AUTHORITY_ENDPOINT` defaulted to `http://127.0.0.1:50110`, an address
   `av-command` has never listened on (its `DEFAULT_BIND` is `127.0.0.1:50070`) — and because the
   fallback is a lazy channel, the miss surfaced only later as a connect error on the first
   `propose_command`, never at startup. Both pre-existing since round 2. The map is now
   av-command 50070/50170, av-gateway 50071/50171, the `+100` convention both binaries already
   followed.

Three more, each an in-flight process defect rather than a code one:

- **An intermittent test failure that passed for the worker and failed in review.** All thirteen
  tests in `av-gateway`'s `mcp` module derived one temp ledger directory from the process id and
  a *constant* name, and `cargo test` runs them concurrently, so two tests interleaved
  `remove_dir_all` with the other's `Ledger::open` and the loser failed with `EEXIST`. Latent
  since round 2; intermittent by construction, with nothing in the failure naming the shared
  path — the same shape as question 199's own intermittent panic. The helper now folds in a
  monotonic per-call counter.
- **A worker wrote its gate logs into the repository worktree** (`scratchpad/` at the repo root),
  which would have been committed. Moved out; every brief since names the scratchpad path
  explicitly.
- **A worker's root cause was wrong and the manager could not reproduce it.** R3.3 attributed a
  container `apt-get update` failure to a deprecated `apt-key`/`gpgv` wrapper and stated it would
  block the edge and cFS image builds "equally if run right now". It does not — see the host
  measurement below. Recorded because a confidently-stated wrong root cause is more expensive
  than an open question.
### Open items for the lead

1. **The user's decision on the Colima VM disk is now blocking work, not just annoying it.**
   Until the dangling volumes are reclaimed (or Kubernetes is disabled in Colima, or the VM disk
   is raised), no AltaVista image that needs the Rust toolchain image can be built on this host:
   the pull itself takes the filesystem to 0 bytes and the build's own `apt-get update` then
   fails on truncated downloads. The proposer's egress proof is written and cannot be run.
2. **The workspace's `rust-version = "1.85"` is false.** `regorus 0.12.0`, pinned since round 1,
   needs a `const fn` stabilised later. Either raise the declared MSRV to a version that
   actually builds this workspace, or the pin is a trap for the first CI or container build that
   honours it.
3. **`av-gateway` authenticates no caller on any surface.** This round gave the command
   authority real service principals; the gateway still takes `caller_clearance` and the
   proposing `principal` as bare strings. Whether that is acceptable under question 155's
   loopback-plus-nginx posture, or whether the gateway needs the same OIDC gate, is a decision
   rather than an oversight — it is now a named Gap row in `docs/compliance/av-gateway/`.
4. **Decision 5 (a second service, `ModelProposeService`, rather than a propose RPC on
   `DataGatewayService`) and decision 8 (the explicit non-loopback bind opt-in, and the reason
   the namespace-sharing alternative was rejected) both need ratifying.** Decision 8 in
   particular is the one place this round deliberately loosens question 155, under a flag, for
   one deployment shape.
5. **No `ListAgents` tool exists in this environment**, so the "run ListAgents before spawning"
   rule could not be followed literally. Workers were dispatched strictly one at a time and each
   result was reviewed before the next was spawned, which gives the same guarantee; recorded so
   the rule can be restated in terms of what the environment actually provides.
6. **A worker created a background-task suggestion chip** (`task_a2b443d0`, "root-cause the apt
   failure") while investigating R3.3. The manager has since root-caused that failure to the VM
   disk, so the chip is stale; the lead or the user may dismiss it.
7. **`av-command`'s `DEFAULT_BIND` is `127.0.0.1:50070`, which is also the port
   `tests/test_edge_plugin_container.py` binds inside its containers.** Not a conflict today
   (different network namespaces) but the two tracks are converging on one host's port space and
   a single owned map would be cheaper than the next collision. This round's own collision
   (defect 9) is the warning.
8. **`Expire`/`Fail` state-machine refusals still write no audit line** — only identity and
   authorization refusals do. Deliberately not widened this round; it is the next increment of
   the same rule if the lead wants it.

## Status (AI-plane manager, 2026-09-13) — round 4, PAUSED

**The user stopped work partway through the consolidation round.** Three of the round's six
planned tasks landed and were reviewed; no worker was running when the round was paused, and
the worktree has no uncommitted edits. Round 4's charter was question 209 (the lead's browser
drive of the console, which found the human step cannot complete) plus question 208's
remaining items.

| Commit | Task | State |
|---|---|---|
| `cb8b4ed` | R4.1 — question 209(a): `Check` runs automatically inside `Propose` on every surface | landed, reviewed |
| `bf4fcfa` | R4.2 — question 209(b) and (c): the console lists CHECKED commands, refreshes counters after every authorize, and an empty scenario no longer aborts `loadScenario` | landed, reviewed |
| `f3f7f69` | R4.2b — the manager's own review defect: the in-memory index is committed before the audit write, not after | landed, reviewed |

### Gates, the manager's own runs with no worker active

Run at `f3f7f69` unless noted. The full round-end gate set was **not** run — the round was
paused, and the three commits' own gates were each verified by the manager at the time.

| Gate | Result |
|---|---|
| `cargo test -p av-command -p av-gateway -p av-proposer` | **287 passed**, 0 failed, 0 ignored (281 was this round's measured baseline at `2c2387b`; +3 from R4.1, +3 from R4.2b) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean, **0 warnings**. `grep -rn "#\[allow" crates/av-command/src crates/av-gateway/src crates/av-proposer/src` is zero hits |
| `cargo test --workspace --exclude av-kernel --no-fail-fast` | **801 passed**, 0 failed, 3 ignored at `f3f7f69` (the worker's run; 795/3 was the manager's measured baseline at `2c2387b`) |
| `.venv/bin/python -m pytest -q -rs` | **545 passed, 5 skipped** (539/5 was the measured baseline), every skip printing its reason — the four pre-existing image gates plus the proposer's container test |
| `tests/test_viewer_net.py` (headless Chrome) | **3 passed**, including the new empty-scenario check, verified by the manager to RUN on this host, not skip |
| `cargo deny check` | `advisories ok, bans ok, licenses ok, sources ok`, six accepted spoore wildcard warnings (question 207) |

Baselines were measured by the manager at `2c2387b` before any worker started: trio 281,
workspace 795/3 ignored, pytest 539 passed / 5 skipped. Full gate output is in the round's
scratchpad.

### The manager's decisions this round

1. **`Propose` runs the check as a second, separate ledger append, never a folded one.** The
   `PROPOSED` record lands and is durable first; only then does the same shared helper the
   explicit `Check` RPC calls run the policy edge. The ledger therefore still holds two
   records with two principals (`"model-x"`, then `"policy"`) and the full five-state trail
   `PROPOSED → CHECKED → AUTHORIZED → DISPATCHED → ACKED` is pinned by a test, so ruling
   209(a)'s "the trail is unchanged" is proven rather than asserted.
2. **A policy denial is a typed `PERMISSION_DENIED` refusal on `Propose` itself**
   (`ServiceError::PolicyDenied`, counter `policy_denied`), not a `200 OK` carrying a
   `REJECTED` command the caller must notice. The `REJECTED` transition and its
   `PolicyDecision` stay durable and queryable — the refusal is on the RPC's return value,
   never on the record. The gateway classifies it from the `tonic::Code` alone
   (`ProposeRefusal::PolicyDenied`, `propose_policy_denied`), not from message prose, which is
   strictly better than the two pre-existing `INVALID_ARGUMENT` siblings that must sniff text.
3. **An automatic-check I/O failure leaves the command `PROPOSED` and says so in words.** The
   explicit `Check` RPC is kept for exactly that retry path, and is refused
   `FAILED_PRECONDITION` naming the actual current state for `CHECKED`, `REJECTED` and
   `AUTHORIZED`. The alternative — returning `Ok` with a bare `PROPOSED` command — was
   rejected: it tells the caller the proposal succeeded while policy has not run.
4. **The console lists commands *awaiting a human*, not "proposals".** Both `PROPOSED` and
   `CHECKED`, via two real per-state `Query` calls per entity (`QueryByEntity` takes exactly
   one `CommandState`, and this needed no proto change), each row carrying the real
   `CommandState` name read off the `Command` itself — never inferred from which of the two
   queries produced it. The HTTP path `/api/command/proposals` is unchanged because the panel
   and its tests depend on it.
5. **`scene.js` gained a WebGL-free pure half.** `resolveFrameGraphInput(sc)` resolves the
   origin frame id, the frame-definition list and a `warning` string; `_buildFrameGraph` calls
   that one function and does the `console.warn` itself. Same split, and the same reason, as
   `trajectoryRenderPositions` — a plain `node` check can now exercise the degraded shapes
   without a `THREE.WebGLRenderer`.
6. **The panel's authorize path is proven without inventing a browser dependency.** The
   headless node check clicks the *real* button `render()` built and captures the exact
   `(commandId, token)` pair the panel hands to `onAuthorize`; pytest then replays those exact
   captured pairs through the real `/api/command/commands/{id}/authorize` route against a real
   `av-command` — wrong-role first (403, the real role-gate reason), right-role second (200,
   the command advances to `AUTHORIZED`). The panel's own control produced the arguments, and
   those arguments really authorize.
7. **The index is committed the instant the ledger append succeeds, before the audit write.**
   The documented rule "an audit line is never written for a transition this crate cannot also
   prove it retained" is kept; what moved is the index commit, the idempotency-key insert and
   the dispatch side effect, all of which had been placed *after* a best-effort log write. See
   the defects below for what that was actually costing.

### What this round actually is now

A proposer, the MCP tool, `ModelProposeService` and a human calling `Propose` directly all get
the policy decision automatically, as a separate logged transition, and a denial comes back
typed and counted on every one of those surfaces through one shared implementation. The
console lists the commands a human can actually act on, shows each one's real state, and
refreshes the counters after a refusal as well as a success — the case where a refusal counter
is the only thing that changed. An execution-profile scenario with no bodies and no frames
loads without throwing, and a real headless-Chrome check proves `loadScenario` reaches the
command-console refresh by reading the real command's real rationale out of the real page's
DOM.

### Declared gaps, all deliberate

- Everything in round 3's "declared gaps" section still stands except the console's inability
  to complete the human step, which this round closed.
- **The panel still does not poll.** Proposals and counters refresh on selection, on every
  authorize outcome, and on an execution-profile scenario load; a new proposal still does not
  arrive on its own.
- A scenario with `frames` but no `frame` has its frame definitions dropped with a
  `console.warn`, rather than one of them being guessed as the root.

### Defects found in review, with their root causes

Two, and the shape held for the fourth round running — **a failure, refusal or guarantee that
leaves no trace**:

1. **The in-memory index was committed after the audit write, so an audit-sink failure
   silently forked the service from its own ledger** (found by the manager reviewing R4.1;
   pre-existing since A2.2 and widened by R4.1's automatic check). The documented ordering rule
   is about the *audit line*, and it is right; but `put_command`, the idempotency-key insert
   and `DispatchSink::dispatch` had all been placed after it too, which quietly made a
   best-effort log the gate on committing state the ledger had already made durable. Two
   demonstrated consequences: after an audit failure on the automatic check, the ledger held
   `PROPOSED, CHECKED` while the index still said `PROPOSED`, so an explicit `Check` retry was
   accepted and appended a **second** `CHECKED` record with a different decision id — the trail
   A6's replay reproduces, now self-contradictory; and in `Dispatch`, the ledger said
   `DISPATCHED` while the asset had never received it and the idempotency key was never
   recorded, so a retry really dispatched and appended a second `DISPATCHED` record, defeating
   A3's "an idempotency key the binding never dispatches twice". Root cause: no invariant
   anywhere tied the index to the ledger, and no test could fail the audit sink, so the
   ordering was argued about rather than exercised. Fixed by making the append and the index
   commit one function no caller can half-use, and by adding a feature-gated failing line sink
   (absent from a default build, proven with `nm`) so both paths are now tested for real. Both
   new tests were watched failing against the unfixed code before the fix landed.
2. **The headless-Chrome console-error collector never watched `Runtime.exceptionThrown`**
   (found by the R4.2 worker while proving its own check had teeth). Question 168's
   "zero console errors on load" gate — the one that should have caught the very crash
   question 209(c) describes — watched only `Log.entryAdded` and `Runtime.consoleAPICalled`.
   An uncaught synchronous exception inside a page event handler is delivered by Chrome on
   `Runtime.exceptionThrown` **and on no other event**, so the real, 100%-reproducible
   `TypeError` in `_buildFrameGraph` produced `errors == []`: a false pass on the exact gate
   that existed to catch it. Root cause: the collector was built against the two failure
   shapes question 168's own investigation had captured live (a failed WebSocket, a 404
   resource) and was never tested against a page that throws. Fixed for every test in that
   file, and the fix is pinned by capturing the real pre-fix stack trace.

One more, an in-flight process defect rather than a code one:

- **A worker raced two `cargo test` invocations against one target directory** (one
  auto-backgrounded on a timeout, then re-run in the foreground) and reported a corrupted
  count with interleaved output before catching it. It also saw one real-clock `av-ingest`
  mTLS test fail under the resulting host load and pass in isolation. Recorded because the
  contention rule exists for exactly this, and because the first symptom was a wrong number,
  not an error.

### What was in flight and what remains

Nothing was in flight when the round was paused — the third worker had finished and been
reviewed. Three of round 4's planned tasks were **not started**:

1. **Question 208(b): `av-gateway` authenticates every caller.** A service subject for the
   proposer, a human token for a console or MCP client, both through `av-command`'s existing
   OIDC verifier shared as a library rather than duplicated; every refusal typed and counted;
   the proposer's container presenting its service token; the Gap row in
   `docs/compliance/av-gateway/` closing to Met with its evidence command. This is still the
   largest open item on the track: the gateway authenticates **no** caller on any surface
   today, and both control matrices say so.
2. **Question 208(a) and (c): the MSRV and the port map.** `rust-version = "1.85"` in
   `Cargo.toml` is still false — `regorus 0.12.0` needs a `const fn` stabilised later, so the
   declared MSRV has been wrong since round 1 and any CI or container build that honours it
   fails. Raising it requires measuring the lowest toolchain that compiles `av-command`
   (`cargo +<version> check -p av-command`), which needs a one-time rustup toolchain install.
   And one owned port map in `docs/architecture.md` (service, gRPC default, admin default)
   with every `DEFAULT_BIND` citing it and a test that the constants and the table agree —
   round 3's defect 9 was that collision arriving, and the survey needed here spans
   `av-command` 50070/50170, `av-gateway` 50071/50171, `av-dynamics-service` 50062/50162,
   `av-lockstep-shim` 50080, and the `container.address` example in the kernel's binding
   registry, which is also `50070`.
3. **The capacity items**, neither started: replacing `oidc.rs`/`policy.rs`'s prose-match
   dependence on regorus's error text with a structural check if `regorus 0.12.0` exposes one
   (question 203(b)'s tripwire otherwise stands), and extending the A5 headless checks with a
   replay of a run driven through the console's authorize, its trail compared.

### Open items for the lead

1. **Round 4 is paused, not finished.** The three landed commits are self-consistent and every
   gate the manager ran at `f3f7f69` is green, so `aiplane` is in a mergeable state — but the
   full round-end gate set (`cargo test -p av-kernel`, `buf breaking` against the merge base)
   was not re-run at `f3f7f69`. No proto file was touched after `cb8b4ed`, whose own `buf
   breaking` run was clean, and no kernel file was touched at all this round.
2. **Question 208(b), (a) and (c) are all still open**, as listed above. 208(b) in particular
   is a security item the lead has already ruled on.
3. **Ruling 209 is fully implemented and the human step now completes end to end**, proven by
   a real headless browser against a real service: an empty publish loads cleanly, the console
   lists a CHECKED command, and the panel's own Authorize button produces the arguments that
   really authorize it with the right role and are really refused with the wrong one. A
   re-drive by the lead in a real browser would be the natural acceptance step.
4. **Question 168's browser gate was passing for the wrong reason** until this round (defect 2
   above). Worth a platform lesson beside round 3's `debug_assert` and TAI-epoch findings: a
   detector is not a gate until it has been shown to fail against a real instance of the
   failure it exists to catch.
5. **The Colima VM disk is still full** and the proposer's container test still skips visibly.
   Nothing this round changed that; it remains the user's decision (question 196(d)).
