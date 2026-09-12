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
