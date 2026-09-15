# Proposals for the user's `secdeploy`

`secdeploy` (`/Users/probe/code/secdeploy`) is run, never vendored or edited (question 213(a)):
AltaVista does not fork it, does not carry a patched copy of any of its files, and merges its
own component fragment into the user's manifest with `deploy/secdeploy/merge.py` instead. This
document is where a change to secdeploy ITSELF would actually help — proposals, not a wish
list, each backed by something reproduced against the real checkout.

## Proposal 1: manifest-declared tiers

### The problem, reproduced

`src/secdeploy/manifest.py` hard-codes the set of placement tiers a component may declare:

```python
# src/secdeploy/manifest.py, line 19
TIERS = {"identity", "inference", "gateway", "collab", "edge"}
```

`Manifest.validate` (same file) rejects anything outside that set, fail-loud, one line per bad
component:

```
component 'av-ingest': tier must be one of ['collab', 'edge', 'gateway', 'identity', 'inference'], got 'engine'
```

That exact text is real output: `tests/test_suite_declarations.py::test_adr003_tiers_are_still_rejected_upstream`
merges the AltaVista fragment into the user's own `suite.toml` WITHOUT the compatibility
mapping described below, runs the real `secdeploy ... verify` against it, and asserts on this
message. As of this writing that test passes because the rejection is real — `verify` returns
exit code 1 and stderr contains four such lines, one per AltaVista component whose ADR-003 tier
isn't in secdeploy's five.

### The tier list that needs it

AltaVista's own substrate design (`docs/adr/003-substrate-and-deployment.md`) declares nine
tiers of its own: `log` (Redpanda), `analytics` (ClickHouse), `store` (MinIO), `catalog`
(Postgres), `engine` (tracking nodes), `kernel` (simulation), `design` (GMAT service, replay,
tiler, scene service), `edge` (edge nodes with plugins), `training`. Only `edge` and `gateway`
(not in that ADR-003 list, but this fragment's own vocabulary for av-command/av-gateway/
av-proposer) happen to share a name with one of secdeploy's five. The rest — `log`, `analytics`,
`store`, `catalog`, `engine`, `kernel`, `training` — have no home in `TIERS` at all. Today only
`engine` and `design` are in play (this round's D1 fragment); as more of ADR-003's tiers get a
secdeploy-fragment component, this gap only widens.

### What we propose

Add one optional, additive key to `suite.toml`'s top level:

```toml
# suite.toml
tiers = ["identity", "inference", "gateway", "collab", "edge"]   # optional; this is the default
```

`Manifest.load` would read it (defaulting to exactly today's `TIERS` constant when absent, so
every existing `suite.toml` — including every deployment that has never heard of this key —
keeps behaving identically), and `Manifest.validate`'s tier check would validate against
`self.tiers` (the loaded set) instead of the module-level constant. `src/secdeploy/topology.py`
already takes its tier vocabulary from `from .manifest import TIERS` for `Topology.validate`'s
own "unknown tier" check (`group {tier!r}: unknown tier`) and for `Topology.single_host`'s
`groups={tier: [name] for tier in TIERS}` — both call sites would read `manifest.tiers` (an
instance, now manifest-scoped) instead of the module constant, so the two validators (manifest
and topology) never disagree about what a tier is.

### Why this is additive and backward-compatible

- Absent the key, `Manifest.load` produces exactly today's five-tier `TIERS` set — no existing
  `suite.toml`, `topology.toml`, or `secsite.toml` changes behavior.
- The validation *shape* is unchanged: still a hard-coded, closed set per manifest, still
  checked the same way, still a `ValueError` with the same message format — just sourced from
  the loaded manifest instead of a module constant. No new failure modes, no relaxed validation.
- `topology.py`'s only two consumers of `TIERS` (the "unknown tier" check and
  `single_host`'s tier enumeration) both already take a `Manifest` as an argument or bound
  field, so threading `manifest.tiers` through is a signature-compatible change, not an API
  break.

### What it would let us delete

`deploy/secdeploy/suite.altavista.toml`'s entire `[tier_compat]` table, and the `map_tiers`
parameter on `deploy/secdeploy/merge.py`'s `render_components`/`merge` (and the CLI's
`--no-map-tiers` flag, which exists purely to prove this gap). With manifest-declared tiers,
the fragment would declare `tiers = ["identity", "inference", "gateway", "collab", "edge",
"engine", "kernel", "design", "log", "analytics", "store", "catalog", "training"]` (secdeploy's
five plus whichever of ADR-003's nine are in play) directly in the merged `suite.toml`, and
every component's `tier` field would be its real ADR-003 tier, unmapped, un-translated, with
nothing to keep in sync between two vocabularies.

## Scope

This is the one proposal this round produced enough evidence to write down. It is intentionally
narrow — it does not attempt to also propose, e.g., a `tiers` key on `topology.toml`/
`secsite.toml` beyond what `Topology`'s existing `unknown tier` check already validates, or any
change to `TARGET_KINDS`/`COMPONENT_KINDS`, neither of which this round's fragment needed to
stretch.
