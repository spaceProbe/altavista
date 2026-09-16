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

## Proposal 2: `secdeploy evidence`'s `collect()` should iterate the manifest, not a module constant

### The problem, reproduced

`src/secdeploy/evidence.py` line 39 hard-codes the entire set of components this collector will
ever probe:

```python
# src/secdeploy/evidence.py, line 39
COMPONENTS = ("secrouter", "seccert", "secllm", "secchat", "secrecorder")
```

`collect()` (defined line 116) loops over it directly:

```python
# src/secdeploy/evidence.py, line 132
    for name in COMPONENTS:
```

That is real output, not a hypothetical: `src/secdeploy/cli.py`'s own `cmd_evidence` (lines
396–414) already loads the manifest one call before it needs this fixed list —

```python
# src/secdeploy/cli.py, lines 399, 406–407
    m = Manifest.load(args.manifest)
    ...
    urls = site.topology.urls(without)
    result = evidence_mod.collect(urls, args.out, token=args.token, timeout=args.timeout)
```

— and `m` is never passed to `evidence_mod.collect` at all; only the already-narrowed `urls` dict
is. So no matter what a merged manifest declares, `collect()`'s own loop can only ever ask about
`secrouter`/`seccert`/`secllm`/`secchat`/`secrecorder`.

The one-line irony, also reproduced directly rather than assumed: `Topology.urls` (the function
that builds the `urls` dict `collect()` already receives) is ITSELF manifest-driven —

```python
# src/secdeploy/topology.py, line 354
        for name, c in self.manifest.select(without).items():
```

— so the `urls` dict handed into `collect()` already reflects whatever the active manifest
declares. `collect()`'s own outer loop is the one place in this path that does not ask the
manifest anything; it asks a five-name constant instead, then does `urls.get(name.upper())` for
each. A component present in the manifest and in `urls` but absent from `COMPONENTS` is simply
never looked at — not even to be recorded `"not_in_topology"`.

This is precisely why AltaVista's own `scripts/kit/evidence.py` (this repository's D4 offline
half — see `docs/compliance/BUNDLE.md`) exists as a *separate* collector rather than something
`secdeploy evidence` could eventually absorb by config alone: `av-command`, `av-dynamics-service`,
`av-edge-plugin`, `av-gateway`, `av-ingest`, `gmat-service` — none of AltaVista's six components
share a name with any of secdeploy's five — so even a fully successful, fully authorized
`deploy/secdeploy/merge.py` merge, run against a live placement that includes an AltaVista
component with a real `/admin/api/evidence` endpoint of its own, could never make `secdeploy
evidence` collect it. The gap is `COMPONENTS` itself, not anything about how the merge is done.

### What we propose

Give `collect()` the same manifest-selection call `Topology.urls` already makes, instead of the
module constant, additively:

```python
# src/secdeploy/evidence.py -- collect()'s new signature
def collect(
    urls: dict[str, str], out_dir: str | Path, *,
    manifest: "Manifest | None" = None, without: list[str] | None = None,
    token: str | None = None, timeout: float = DEFAULT_TIMEOUT, today: date | None = None,
) -> dict[str, object]:
    component_names = (
        sorted(manifest.select(without)) if manifest is not None else COMPONENTS
    )
    for name in component_names:
        ...  # unchanged body
```

`cmd_evidence` (`src/secdeploy/cli.py`) would then pass the `m`/`without` it already has in
scope one call earlier than today:

```python
# src/secdeploy/cli.py, cmd_evidence -- the one-line call-site change
    result = evidence_mod.collect(
        urls, args.out, manifest=m, without=without, token=args.token, timeout=args.timeout,
    )
```

`manifest.select(without)` is the exact call `Topology.urls` already makes internally
(`topology.py` line 354) to build `urls` in the first place — so after this change, the set of
components `collect()` asks about and the set `Topology.urls` resolved addresses for are
guaranteed to agree (both are `manifest.select(without)`, called with the identical `without`),
rather than merely overlapping by coincidence the way `COMPONENTS ⊂ {secrouter, seccert, secllm,
secchat, secrecorder}` happens to today.

### Why this is additive and backward-compatible

- `manifest`/`without` are both optional, defaulting to `None`. Absent `manifest`, `collect()`
  falls back to exactly today's `COMPONENTS` tuple — any existing caller (including
  `tests/test_suite_declarations.py`-style tests that call `collect()` directly without a
  `Manifest` in hand) keeps its current behavior unchanged.
- The per-component probing logic itself (`fetch_one`, `resolve_token`, the `"skipped"`/
  `"error"`/`"not_in_topology"`/`"ok"` tolerance) is untouched — only WHICH names are looped over
  changes, never how each one is handled once named.
- `COMPONENTS` itself is not removed; it stays the documented default for a caller with no
  manifest at hand, exactly as it is used today.

### What it would let us delete

Nothing in this repository — `deploy/secdeploy/merge.py`/`suite.altavista.toml` do not touch
`evidence.py` at all today, unlike Proposal 1's `[tier_compat]` table. The benefit is purely
forward-looking: the moment any AltaVista component grows a real `/admin/api/evidence` endpoint
of its own (this round's `gmat_service.evidence.EvidenceLog`/`crates/av-dynamics-service/src/
evidence.rs` are already real, hash-chained, file-backed ledgers with real verifiers — an HTTP
`/admin/api/evidence` surface over either is a small step, already true for `gmat_service.admin`
and `av-dynamics-service`'s own admin surface today), `secdeploy evidence` run over a live
placement that includes it would collect that component's evidence for real, with no further
code change on either side — which is exactly the gap `scripts/kit/evidence.py`'s own
`ledger_verify.live` declared, not-yet-collected slot (`docs/compliance/BUNDLE.md`) is waiting
for a second worker to fill from the live side.

## Proposal 3: `deploy()` should walk the manifest, not a hard-coded services tuple

### The problem, reproduced

`src/secdeploy/targets/fedora_fips.py` line 60 hard-codes the entire set of components any
`fedora-fips` deploy will ever install:

```python
# src/secdeploy/targets/fedora_fips.py, line 60
SERVICES = ("secdns", "seccert", "secllm", "secrouter", "secagent", "secrecorder", "secproxy")
```

`deploy()`'s own placement filter (lines 525–538) intersects this fixed tuple with whatever the
topology actually placed — it never asks the manifest what else is on this resource:

```python
# src/secdeploy/targets/fedora_fips.py, lines 525–538
    def _include(svc: str) -> bool:
        if svc in without:
            return False
        if svc == "secdns" and topology is None:
            return False
        if svc == "secllm" and (topology is None or not with_inference):
            return False
        if svc == "secagent" and (topology is None or not with_agent):
            return False
        if svc == "secproxy" and topology is None:
            return False
        return placed is None or svc in placed

    services = [s for s in SERVICES if _include(s)]
```

`targets/macos.py` has the identical gap, at its own `deploy()` (lines 877–885) — not a module
constant there, an inline literal with the same seven names:

```python
# src/secdeploy/targets/macos.py, lines 877–885
    services = [
        n for n in ("secdns", "seccert", "secllm", "secrouter", "secagent", "secrecorder", "secproxy")
        if n not in without
        and (n != "secdns" or topology is not None)
        and (n != "secllm" or (topology is not None and with_inference))
        and (n != "secagent" or (topology is not None and with_agent))
        and (n != "secproxy" or topology is not None)
        and _here(n)
    ]
```

This is real output, reproduced by `tests/test_suite_declarations.py::
test_secdeploy_deploy_fedora_fips_dry_run_renders_nothing_for_our_components` (P5 round 3, D6 —
see `docs/compliance/fedora-fips.md`): `secdeploy --manifest <our merged manifest> deploy
fedora-fips --dry-run --site <our eval site>` renders a full plan — but every step in it names
one of the seven `SERVICES` entries, never `av-ingest`/`av-command`/`av-gateway`/`av-proposer`/
`av-dynamics-service`/`gmat-service`/`av-edge-plugin`/`av-viewer`, even though all eight are
declared components in the manifest `deploy()` was handed and are placed on the deploying
resource by the topology. The one partial exception is a side effect of a DIFFERENT,
manifest-driven code path: `av-viewer` (the only AltaVista component with `fronted = true`) gets
named as a `-d` flag in secproxy's certbot SAN-cert command, because `wiring.fronted_instances`
(which builds that flag list) reads `topology.manifest.select(without)` directly — the whole
manifest, not `SERVICES` — while every other step of `deploy()` (code install, config file,
systemd unit, state dir, system user) still never mentions it. `av-viewer` gets a TLS name and
nothing else; the other seven get nothing at all.

### What we propose

The same shape as Proposal 1's fix, applied to `deploy()` instead of `Manifest.validate`: derive
`SERVICES` from the manifest's own components (filtered to `kind = "service"`, the ones this
native-systemd/launchd path knows how to install) rather than a fixed tuple, with the eight
named native components (`secdns`, `seccert`, `secllm`, `secrouter`, `secagent`, `secrecorder`,
`secproxy`, and macOS's own local-only entries) kept as the ones with a real installer (checkout
copy, env template, systemd/launchd unit) and everything else in the manifest still eligible for
the parts of `deploy()` that are already manifest-driven today — the addressing/DNS zone, the
audit artifact's component count, and (as `av-viewer` already proves) the fronted-FQDN set. Short
of that generic dispatch, even a documented "manifest components outside `SERVICES` are placed
but not installable on this target" `P.warn()` at the top of `deploy()` would turn today's
*silent* gap into a *reported* one.

### Why this is additive and backward-compatible

- Every one of secdeploy's own five targets ships a fixed, known-shape set of native units
  (systemd files, launchd plists) that live in this repository's own `deploy/<target>/` tree —
  there is no generic "install this manifest component's code + unit" machinery to hook into yet,
  so a full fix is a bigger change than Proposals 1/2. The additive, low-risk piece is only the
  `P.warn()` above: it changes no installed behavior, only what operators are told.
- `SERVICES`/the inline macOS tuple stay exactly as they are today for every component secdeploy
  itself ships; nothing about their behavior changes.

### What it would let us delete

Nothing in this repository yet — `docs/compliance/fedora-fips.md` and
`scripts/kit/evidence.py`/`scripts/kit/live_evidence.py` (D4/D4b, see `docs/compliance/BUNDLE.md`)
remain the actual mechanism by which our own components' evidence/health gets collected on a
placement that includes them; this proposal is what would eventually let a real `fedora-fips`
*deploy* (not just its dry-run render) stand our components up through secdeploy itself, which
nothing today does or claims to do.

## Scope

Three proposals across two rounds produced enough evidence to write down. All are intentionally
narrow — Proposal 1 does not attempt to also propose, e.g., a `tiers` key on `topology.toml`/
`secsite.toml` beyond what `Topology`'s existing `unknown tier` check already validates, or any
change to `TARGET_KINDS`/`COMPONENT_KINDS`, neither of which this round's fragment needed to
stretch; Proposal 2 does not attempt to also propose that `secdeploy evidence` itself learn to
probe an AltaVista-shaped `/admin/api/evidence` response body (its shape already matches
`ChainVerification`'s `ok`/`checked`/`broken_at_seq`/`detail` fields closely, per
`gmat_service.evidence.EvidenceLog.verify`'s own doc, but reconciling that with SecRouter's own
`/admin/api/evidence` response shape is a separate, unreproduced question this round did not
investigate) or that `evidence.collect`'s five-second-per-component `DEFAULT_TIMEOUT` needs
tuning for a mixed suite -- neither is evidenced here, so neither is proposed here; Proposal 3
(P5 round 3, D6) does not attempt to design the generic per-manifest install/unit-generation
machinery that a full fix would need (there is no evidence yet for what that would look like
across five very different targets), only the narrow, reproduced gap and the one low-risk,
additive step (a `P.warn()`) available short of that larger redesign.
