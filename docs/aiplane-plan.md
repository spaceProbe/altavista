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
- **`Query` does not survive a restart.** The command index is in memory; the ledger record
  does not carry enough of a `Command` (no payload, deadline, label or provenance) to
  rebuild one. Closing it is a ledger-shape decision: widen `LedgerRecord`, or add a command
  store. Recorded in `service.rs` and as a control-matrix deficiency.
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
