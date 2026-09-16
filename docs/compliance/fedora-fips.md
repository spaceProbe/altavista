# D6 — the `fedora-fips` target

`docs/p5-plan.md` D6: run `secdeploy deploy fedora-fips --dry-run` over our own merged manifest
and evaluation site, render the runbook + systemd units for our eight components, run the real
FIPS preflight inside a Lima Fedora VM if one will boot on this host, and record — with the real
command and the real output, never an exit code alone (question 148) — what could not run and
why. This document is that record. It is committed; the render output it quotes from lives under
`out/d6-fedora-fips/` (gitignored, not committed — see `.gitignore`'s `/out/` entry).

## The dry-run render

```
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export GMAT_ROOT="/Users/probe/code/AltaVista/GMAT R2026a"
export CFS_MIRROR_DIR=/Users/probe/code/AltaVista/third_party/mirrors
.venv/bin/python deploy/secdeploy/merge.py \
  --out out/d6-fedora-fips/merged \
  --site deploy/secdeploy/secsite.altavista-eval.toml

uv run --offline --project /Users/probe/code/secdeploy secdeploy \
  --manifest "$(pwd)/out/d6-fedora-fips/merged/suite.merged.toml" \
  --work "$(pwd)/out/d6-fedora-fips/work" \
  --out "$(pwd)/out/d6-fedora-fips/secdeploy-out" \
  deploy fedora-fips --dry-run \
  --site "$(pwd)/out/d6-fedora-fips/merged/secsite.merged.toml"
```

Exit code `0`. This is the exact same `merge.merge(...)` + `_uv_run(...)` shape
`tests/test_suite_declarations.py` already uses for `verify`/`plan macos`, applied to `deploy
fedora-fips --dry-run` — see `tests/test_suite_declarations.py::
test_secdeploy_deploy_fedora_fips_dry_run_renders_nothing_for_our_components`, which runs this
for real (offline, gated on the same `requires_secdeploy` visible-skip guard as every other
secdeploy test in that file) and asserts on the render quoted below.

`deploy/secdeploy/secsite.altavista-eval.toml` declares one resource (`local`, `target =
"macos"`) — `wiring.resource_for` falls back to "the only resource" when none declares `target =
"fedora-fips"` (`src/secdeploy/wiring.py` line 121: `if len(topology.resources) == 1: return
next(iter(topology.resources))`), so the mismatched `target` field is harmless: it only affects
resource *selection* when more than one resource is declared, never which target module
`deploy()` dispatches to (`cli.py`'s `_target_mod(args.target)` reads the CLI's own `deploy
fedora-fips` argument, not the site file).

### What rendered

The plan header:

```
# fedora-fips deploy plan — suite 2.0.0 (run as root on the Fedora host)
# topology: resource 'local' @ 127.0.0.1 — native services here: secdns, seccert, secrouter, secrecorder, secproxy
# addressing: writes secdns zone/env + peer env/ (also via `bundle fedora-fips --resource local`)
```

37 steps followed — FIPS preflight, per-service system users + state dirs, code installs to
`/opt/secsuite/{secdns,seccert,secrouter,secrecorder,secproxy}`, env files, the secdns
zone/env, SecRouter's egress allow-list + addressing env, SecRecorder's addressing env, the
nginx/certbot runtime install, secproxy's cert dir/ACME webroot/tmp dir/www root, the generated
nginx config + logrotate config + landing page + error pages, the secproxy SAN-cert `certbot`
invocation, five systemd unit installs (`secdns`, `seccert`, `secrouter`, `secrecorder`,
`secproxy` — **not** `secllm`/`secagent`, both excluded because no `--with-inference`/
`--with-agent` flag was passed) + `secsuite.target`, `daemon-reload`, `enable --now
secsuite.target`, the SecCert trust-anchor curl+`update-ca-trust` loop, then the two stacks
(`secchat`, `secsso`) and a closing audit-artifact preview line:

```
  · audit: a real run would write .../secdeploy-out/audit/deploy-fedora-fips-local.json (+ .txt) — 7 component(s) on 'local', trust_anchor=yes, resolver=no
  · landing page → https://altavista.internal/
```

(`7 component(s)` = the 5 native services + 2 stacks above — `len(services) + len(stacks)`,
`src/secdeploy/targets/fedora_fips.py` line 635.)

One warning on stderr:

```
! secllm catalog drift check: skipped — no [secllm].catalog set in secsite.toml, so SecRouter's tier bindings / autostart_models can't be cross-checked against a known catalog (secllm's own built-in catalog is assumed on every instance).
```

— expected: `deploy/secdeploy/secsite.altavista-eval.toml` sets no `[secllm]` table, and this
run passed no `--with-inference` either.

### Which of our eight components appeared — and which did not

Grepping the full stdout+stderr for each of `ALTAVISTA_COMPONENTS`
(`tests/test_suite_declarations.py`'s own list — `av-ingest`, `av-command`, `av-gateway`,
`av-proposer`, `av-dynamics-service`, `gmat-service`, `av-edge-plugin`, `av-viewer`):

| Component | Appears? | Where |
|---|---|---|
| `av-ingest` | No | — |
| `av-command` | No | — |
| `av-gateway` | No | — |
| `av-proposer` | No | — |
| `av-dynamics-service` | No | — |
| `gmat-service` | No | — |
| `av-edge-plugin` | No | — |
| `av-viewer` | **Partially** | named in secproxy's certbot `-d` flags only |

Seven of eight components render **nothing at all** — no system user, no state dir, no code
install, no config file, no systemd unit, no mention anywhere in the plan. `av-viewer` is the one
exception, and only partially: it gets a name in the SAN-cert issuance step —

```
  · issue secproxy SAN cert from SecCert via certbot --standalone (--cert-name secproxy, 6 -d names: altavista.internal, secsso.altavista.internal, secrouter.altavista.internal, secchat.altavista.internal, secrecorder.altavista.internal, av-viewer.altavista.internal)
      bash -c certbot certonly --standalone --non-interactive --agree-tos --register-unsafely-without-email --config-dir ... --server http://seccert.altavista.internal:47001/acme/directory --http-01-port 80 --cert-name secproxy -d altavista.internal -d secsso.altavista.internal -d secrouter.altavista.internal -d secchat.altavista.internal -d secrecorder.altavista.internal -d av-viewer.altavista.internal || echo '...'
```

— and nothing else: no `install av-viewer code`, no `av-viewer.service`, no state dir, no
config file. `av-viewer` is the only one of our eight components declared `fronted = true` in
`deploy/secdeploy/suite.altavista.toml` (line 129); everything else about it is exactly as
absent as the other seven.

### Why

`src/secdeploy/targets/fedora_fips.py` hard-codes the entire set of components a `fedora-fips`
deploy will ever touch — a module constant, not read from the manifest:

```python
# src/secdeploy/targets/fedora_fips.py, line 60
SERVICES = ("secdns", "seccert", "secllm", "secrouter", "secagent", "secrecorder", "secproxy")
```

`deploy()`'s placement filter (lines 525–538) only ever intersects this fixed tuple with what the
topology placed — never the other way around, so a manifest component outside `SERVICES` is
simply invisible to every step that builds `steps`/`services`:

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

The one exception traces to a *different*, genuinely manifest-driven code path:
`wiring.fronted_instances` (which builds the certbot `-d` list) reads
`topology.manifest.select(without)` directly — the whole merged manifest — independent of
`SERVICES`, which is why `av-viewer`'s `fronted = true` flag surfaces there and nowhere else.

`src/secdeploy/targets/macos.py` has the identical gap at its own `deploy()` (an inline literal
there, lines 877–885, not even a module constant, with the same seven names) — this is the "same
shape" the task charter predicted, now confirmed by direct read rather than assumed. Both are
written up as Proposal 3 in `docs/secdeploy-upstream.md`, in the same style as the two proposals
already there (exact module, exact line numbers, reproduced behaviour).

### The test that backs this

`tests/test_suite_declarations.py::
test_secdeploy_deploy_fedora_fips_dry_run_renders_nothing_for_our_components` runs the exact
render above for real (offline, `requires_secdeploy`-gated the same way every other secdeploy
test in that file is — question 194: run for real or skip visibly, never pass silently) and
asserts, positively: the native-services header line names exactly secdeploy's own five
placed services; none of the eight components' names appear as `install <name> code` or `install
<name>.service`; `av-viewer`'s FQDN appears in the certbot `-d` flags and nowhere else; every
other component's FQDN appears nowhere at all. The day `secdeploy` gains generic per-manifest
dispatch (Proposal 3 above), this test starts failing — the same role
`test_adr003_tiers_are_still_rejected_upstream` already plays for the tier gap (Proposal 1).
