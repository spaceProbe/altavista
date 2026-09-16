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

## The Lima Fedora VM

`lima` 2.1.4 is on this host, alongside Colima (its VM runs the Docker daemon other tracks
depend on — not touched by anything below). Before this task, `limactl list` reported:

```
No instance found. Run `limactl create` to create an instance.
```

— no cached Lima instance, and no Fedora qcow2/template artifact anywhere under
`~/.lima`/`~/Library/Caches/lima` (checked before fetching anything, per D6's own instruction —
question 154 permits a one-time image fetch at setup, not at test time). `colima status` at the
same moment:

```
colima is running using macOS Virtualization.Framework
arch: aarch64
runtime: docker
mountType: virtiofs
docker socket: unix:///Users/probe/.colima/default/docker.sock
containerd socket: unix:///Users/probe/.colima/default/containerd.sock
kubernetes: enabled
```

### The attempt

```
limactl start --name=fedora-fips-d6 --tty=false template://fedora
```

Lima's own `fedora` template (`/opt/homebrew/Cellar/lima/2.1.4/share/lima/templates/fedora.yaml`
→ `_images/fedora-44.yaml`) resolves to the aarch64 Fedora 44 Cloud Base qcow2 (this host is
Apple Silicon):
`https://download.fedoraproject.org/pub/fedora/linux/releases/44/Cloud/aarch64/images/Fedora-Cloud-Base-Generic-44-1.7.aarch64.qcow2`.

**It booted.** Real timings, read from the hostagent's own log:

| Step | Started (local) | Finished | Elapsed |
|---|---|---|---|
| Download Fedora 44 Cloud qcow2 (aarch64) | 00:01:50 | 00:14:12 | ~12m22s |
| Convert qcow2 → raw disk, expand to 100GiB | 00:14:12 | 00:14:14 | ~2s |
| Download the nerdctl-full archive (the template's default containerd/nerdctl guest tooling — not needed for the FIPS preflight itself, but part of this stock template's own provisioning, not something this task asked for separately) | 00:14:14 | 00:16:35 | ~2m21s |
| Start VZ, SSH ready | 00:16:35 | 00:16:43 | ~8s |

Total wall time from `limactl start` to a working SSH shell: **~14m53s**. Confirmed booted:

```
$ limactl shell fedora-fips-d6 -- bash -c 'whoami; cat /etc/os-release | head -5; uname -a'
probe
NAME="Fedora Linux"
VERSION="44 (Cloud Edition)"
RELEASE_TYPE=stable
ID=fedora
VERSION_ID=44
Linux lima-fedora-fips-d6 6.19.10-300.fc44.aarch64 #1 SMP PREEMPT_DYNAMIC Wed Mar 25 17:45:07 UTC 2026 aarch64 GNU/Linux
```

### The real FIPS preflight, run for real

`docs/fedora-fips.md` §3 names the exact preflight: `deploy/fedora-fips/fips-preflight.sh`. Copied
in and run as root, unmodified:

```
$ limactl copy /Users/probe/code/secdeploy/deploy/fedora-fips/fips-preflight.sh fedora-fips-d6:/tmp/fips-preflight.sh
$ limactl shell fedora-fips-d6 -- bash -c 'chmod +x /tmp/fips-preflight.sh && sudo /tmp/fips-preflight.sh; echo "EXITCODE=$?"'
SecDeploy FIPS preflight
FIPS preflight FAILED: kernel FIPS mode is not enabled — run 'sudo fips-mode-setup --enable' and reboot
EXITCODE=1
```

Real, fail-closed, exactly as `fips-preflight.sh`'s own first check
(`/proc/sys/crypto/fips_enabled`) says it should on a stock cloud image that has never had FIPS
mode enabled — not a crash, not a skip, the documented fail-closed behaviour working as designed.

**Root cause of why it can't be turned green here, run down to the actual binary:** the
preflight's own remediation text — `sudo fips-mode-setup --enable` — names a command that does
not exist on this Fedora 44 image:

```
$ limactl shell fedora-fips-d6 -- bash -c 'command -v fips-mode-setup; rpm -q crypto-policies-scripts; sudo fips-mode-setup --check'
crypto-policies-scripts-20251128-3.git19878fe.fc44.noarch
bash: line 1: fips-mode-setup: command not found
```

```
$ limactl shell fedora-fips-d6 -- sudo dnf provides "*/fips-mode-setup"
No matches found. If searching for a file, try specifying the full path or using a wildcard prefix ("*/") at the beginning.
```

`rpm -ql crypto-policies-scripts` (the package that ships `update-crypto-policies`, confirmed
present and reporting policy `DEFAULT`) lists no `fips-mode-setup` file at all — only
`update-crypto-policies` itself and its Python implementation. This is as far as this task's
budget goes on root-causing the gap: `docs/fedora-fips.md` §1 and `fips-preflight.sh`'s own
remediation message both name a tool that is not installable by that name via `dnf provides` on
Fedora 44 — closing it (finding Fedora 44's actual current FIPS-enablement path, applying it,
rebooting, and re-running the preflight to green) is out of scope for D6's bounded VM attempt
("do not burn the budget trying to make a VM work" — this task's own charter) and is not
attempted further here.

### What cannot run in this VM no matter what

- **Hardware attestation of the FIPS boundary** (e.g. a TPM-rooted remote-attestation quote for
  the host running FIPS mode) — Lima's `vz`-driver VM here has no virtual TPM device in its
  config, and even a virtual one would root its attestation in Apple's Virtualization.framework,
  not an independently-verifiable physical root of trust the way a real Fedora server with a
  discrete TPM would. No Lima VM on any Mac can produce this.
- **A real HSM** — `docs/fedora-fips.md`'s own backup/restore section (`## Backup and restore` →
  "Encryption: public-key, to a recipient cert (private key offline)") names exactly this: "keep
  `backup-key.pem` OFFLINE (an HSM, an air-gapped USB)". This VM (or any Lima VM) has no physical
  HSM attached and none can be attached to a `vz` guest on this host.
- **A real enclave network** — every network path in and out of this VM is Lima's own
  NAT/port-forwarding through the single host Mac's network interface; there is no physically
  segmented, air-gapped enclave network to exercise SecRouter's egress allow-list
  (`SECROUTER_EGRESS_FILE`) or SecLLM pool addressing against — that boundary is only real on
  actual separately-networked hardware.

### Teardown — the host left as found

```
$ limactl stop fedora-fips-d6
... The instance fedora-fips-d6 has shut down
$ limactl delete fedora-fips-d6
... Deleted `fedora-fips-d6` (`/Users/probe/.lima/fedora-fips-d6`)
$ limactl list
No instance found. Run `limactl create` to create an instance.
$ colima status
colima is running using macOS Virtualization.Framework
arch: aarch64
runtime: docker
mountType: virtiofs
docker socket: unix:///Users/probe/.colima/default/docker.sock
containerd socket: unix:///Users/probe/.colima/default/containerd.sock
kubernetes: enabled
```

Both match the before-state exactly (`limactl list` reports the same "no instance" message it
did before this task started; `colima status` is byte-identical). Lima's download cache
(`~/Library/Caches/lima/download/` — 504MiB Fedora qcow2 + 256MiB nerdctl-full archive, ~760MiB
total) was left in place: it is Lima's own reusable setup cache, not a VM instance, and nothing
in D6 asks for it to be cleared.

### Network use, recorded

One-time setup fetch only (question 154), no network at test time: the Fedora 44 Cloud aarch64
qcow2 (504MiB) and the `fedora` template's own nerdctl-full guest-tooling archive (256MiB) — the
latter is part of Lima's stock template provisioning, not something fetched separately for this
task. `dnf` metadata refreshes (~80MiB, inside the VM, while checking for `fips-mode-setup`) are
likewise one-time-setup network use inside a VM that no longer exists. Total: ~840MiB.
