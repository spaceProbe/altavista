# The evidence bundle

D4 (`docs/p5-plan.md`): "`secdeploy evidence` run over a deployed evaluation placement, plus our
own `scripts/kit/evidence.py` that collects what secdeploy cannot know: every component's control
matrix, the cross-component NIST 800-171 coverage table (per practice: Met, Partial, Inherited,
Gap, and which component says so), the deficiency list, the ledger `verify` results, the SBOM
hashes and the kit manifest hash, into one hashed bundle."

This document records what that bundle is, exactly what regenerates it, its own SHA-256 as
measured at this commit, and what that hash depends on. **The bundle itself is not committed** --
see "Where it lives" below.

## Where it lives

`out/evidence/bundle.json`, written by `scripts/kit/evidence.py`. `out/` is gitignored
(`.gitignore`'s `/out/` entry, added for `scripts/kit/build_kit.py`'s own kit output and already
covering this path -- confirmed: `git check-ignore -v out/evidence/bundle.json` names that exact
line; no `.gitignore` change was needed for this task). This mirrors `docs/p5-plan.md`'s own D3
precedent (`KIT_MANIFEST`): a big, regenerable, machine-produced artifact is never committed,
only the command that reproduces it and its own hash are.

This is a deliberate location change from `docs/p5-plan.md`'s own prose, which names
`docs/compliance/bundle/` -- the lead's round-3 charter for this task moves it to `out/evidence/`
instead, and per that charter's own rule ("the lead's later instruction wins"), this is the
authoritative location.

## Regenerating it

```
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
.venv/bin/python scripts/kit/evidence.py --out out/evidence
```

Add `--kit <dir>` to record a kit's own `KIT_MANIFEST` hash (a kit `scripts/kit/build_kit.py`
already built into `<dir>`), and/or `--ledger-dir <dir>` to drive
`gmat_service.evidence.EvidenceLog.verify()` for real against `<dir>/evidence.jsonl`. Neither is
required -- omitted, each becomes a declared, named `"not_collected"`/absent entry in the bundle
rather than a silent hole (see `scripts/kit/evidence.py`'s own top doc, "Bundle shape").

## The bundle's own SHA-256, at this commit

```
2c306b76b89bd1701af0b542843873ebe866ac25813f9020de62777c6a4e0a32
```

Moved here from `459c107c164fc0756cb95c9a5a03659731a3296075e0c60abd45d2b63574d808` by the
native-dynamics track's own round 2 task 5 (question 224, the task this section's own last
paragraph named as still open): `scripts/kit/sbom.py::python_dist_sbom` no longer reads the live
worktree `.venv` directly for the two Python SBOMs' package list -- it reads the new committed
`scripts/kit/python-lock.json` (`scripts/kit/python_lock.py`'s output), cross-checking the live
venv rather than sourcing from it. Regenerating `av-viewer.cdx.json`/`gmat-service.cdx.json`
through the generator with this fix applied dropped `pip` from both (the venv-bootstrap tool
`pip install -e ".[dev]"` always adds regardless of what `pyproject.toml` declares -- nothing in
this project's own dependency graph names it, so the declared set this task's fix reads never
did either) and updated both files' `altavista:sbom:python-packages-source` property text to
describe the new source; `docs/compliance/sbom/SHA256SUMS` changed as a direct result for
exactly those two files, `SHA256SUMS` is one of `epoch_paths` above, and `sbom_hashes` is one of
the evidence-dependent inputs `bundle_sha256` hashes (see "What the hash depends on" below) --
so the bundle regenerated from the current tree no longer matched the previous recorded number,
caught by `test_bundle_sha256_matches_the_hash_recorded_in_bundle_md` and fixed the same way
every prior move in this section was: re-running the documented command and recording what it
actually printed. No control matrix, component set, or non-Python evidence content changed.

Recorded from the verification clone at the commit that regenerated the SBOMs for the merge
of heavy round 2 into the native-dynamics branch (`7ea2d24`, question 220), following the same
two-step the previous value used. The previous value,
`06a1838aed06080ed56a8d9d46a5b131b02c3bbc74f5c3232fb24205e019db3e`, was recorded from the
verification clone at the commit that regenerated the SBOMs (the bundle's
epoch is the last commit touching its inputs, so the SBOM commit itself moved it, and this
record is written in a following commit that touches no input). Moved here, in two steps,
from `c2670059ce354b9233a1e01455bead81c3223c0d919338f6321a8b0f2b899aec` on
2026-09-16 by the lead at the heavy round 2 merge: the two Python SBOMs (`av-viewer`,
`gmat-service`) had been regenerated from a worktree venv that lacks `setuptools` and carries
a stale `altavista` dist-info, so they recorded `setuptools:no-longer-there` and
`altavista:no-licence-metadata`; regenerated from a venv installed with `pip install -e
".[dev]"` (the verification clone's), which is what the committed documents describe. The
finding that a Python SBOM records the live venv rather than the declared dependency set is
question 224.

Measured by running the command above with no `--kit`/`--ledger-dir` (both offline-declared
slots) and reading `bundle_sha256` from the written `out/evidence/bundle.json` -- the same value
the command's own stderr prints.
`tests/test_evidence_bundle.py::test_bundle_sha256_matches_the_hash_recorded_in_bundle_md`
regenerates the bundle from the current tree and asserts its `bundle_sha256` equals this exact
number, parsed out of this file for real -- so this number is load-bearing, not decorative: if it
ever drifts from what the current tree actually regenerates, CI fails until this file is updated.

This number moved from the commit-1 value (`4d55fe50888b...`) because of D4b (commit 2, this
task's live half, `scripts/kit/live_evidence.py`): `assemble_bundle` gained a new top-level
`secdeploy_evidence` field alongside the existing `ledger_verify.live` one, and its own
committed-default value (`SECDEPLOY_EVIDENCE_NOT_COLLECTED`) is new, real evidence content --
exactly the "the hash moves when the EVIDENCE moves" rule this section states below, not a
regression of the round-3 `git_commit` fix (see "What the hash depends on").

It moved again, to the number above, from `19992e76e3938671ae444e917067b713a5beac295e91de0ec1f571ec274c78d6`,
because of the aiplane merge's own SBOM regeneration (commit `4f559a9`, "Regenerate the SBOMs for
the merge's workspace-manifest epoch move (question 220)"): that commit's real content change
(confirmed with `git show --stat`, not assumed from its message alone) was `docs/compliance/sbom/
av-viewer.cdx.json` and `docs/compliance/sbom/gmat-service.cdx.json` losing stale distribution
entries the two Python SBOMs no longer install in the shared worktree `.venv` (`setuptools` is no
longer an installed distribution there at all, and `altavista` itself now carries no captured
licence metadata rather than the `Apache-2.0` an earlier, differently-provisioned `.venv` once
reported) -- `docs/compliance/sbom/SHA256SUMS` changed as a direct result, `SHA256SUMS` is one of
`epoch_paths` above, and `sbom_hashes` is one of the evidence-dependent inputs `bundle_sha256`
hashes (see "What the hash depends on" below): the bundle regenerated from the current tree no
longer matched this file's previously recorded number, caught by
`test_bundle_sha256_matches_the_hash_recorded_in_bundle_md` and fixed by re-running the documented
command and recording what it actually printed. Re-running `scripts/kit/sbom.py --out docs/
compliance/sbom` for `av-viewer`/`gmat-service` against this same tree reproduces
`docs/compliance/sbom/av-viewer.cdx.json` and `docs/compliance/sbom/gmat-service.cdx.json`
byte-for-byte -- confirmed with a real regeneration and `git status`/`diff` showing no change --
so both files, and the bundle hash above, are already consistent with this worktree's real,
current `.venv`; no component set, control matrix, or evidence content changed, only the two
Python SBOMs' `.venv`-derived package list.

## What the hash depends on

Same discipline `docs/compliance/sbom/README.md`'s own "Determinism" section already carries for
the ten committed SBOMs, applied here to the bundle -- **the rule is that `bundle_sha256` moves
when the EVIDENCE moves, never merely because a commit landed.** Concretely, `bundle_sha256` is
computed over the bundle's canonical JSON with exactly two fields excluded: itself, and
`git_commit` (round 3 defect fix -- see below for why `git_commit` had to join that exclusion).
Everything else is evidence-dependent:

- **The six control matrices** (`docs/compliance/{av-command,av-dynamics-service,av-edge-plugin,
  av-gateway,av-ingest,gmat-service}/control-matrix.md`) -- every row, every deficiency, and each
  file's own SHA-256 (which the bundle also records per-component, so a reader can independently
  confirm which exact text a `control_matrices` entry summarised).
- **`docs/compliance/sbom/SHA256SUMS`** and, transitively, the ten `*.cdx.json` files it records
  hashes for (the bundle re-verifies every one against the files on disk -- a stale `SHA256SUMS`
  becomes a recorded `sbom_hashes.stale` finding, not a silent pass-through of the recorded
  value).
- **Whatever `--kit`/`--ledger-dir` content the invocation was given**, and (D4b, new)
  **whatever `ledger_verify_live`/`secdeploy_evidence` content `assemble_bundle`'s caller
  supplied** -- or, given none of the four, the fixed "not given this run"/`"not_collected"`
  placeholders. These are all run-time inputs, not committed files, so they do not move with a
  commit the way the two bullets above do; two regenerations at the SAME commit with the SAME
  four-input state (including "none of them") produce the identical hash
  (`tests/test_evidence_bundle.py::test_two_runs_over_the_same_state_are_byte_identical`;
  `tests/test_live_evidence.py::test_two_bundles_over_the_same_saved_live_input_are_byte_identical`
  extends this to a live-input fixture specifically).
- **`epoch`**, a git-derived timestamp -- never `datetime.now()`; `git log`'s own committer date
  for exactly the paths in the first two bullets (`bundle.json`'s own `epoch_paths` field lists
  them, so a reader never has to re-derive which commits touch them, matching the SBOM README's
  own transparency rule). `epoch` only moves when a commit actually touches one of those paths.

**`git rev-parse HEAD` (`git_commit`) is recorded in the bundle for provenance only -- which
commit this bundle was built against -- and is explicitly EXCLUDED from what `bundle_sha256`
hashes**, exactly the same "a record cannot hash itself" treatment the `bundle_sha256` field
already gives its own value. This is a round 3 code-review fix: `git_commit` used to sit INSIDE
the hashed canonical content, so `bundle_sha256` changed on *every* commit regardless of whether
any evidence input changed -- question 214's round 1 platform lesson ("a committed artifact whose
input set includes its own commit is stale the moment it lands"), already fixed once for the
SBOMs (round 1 decisions 6 and 7) and now fixed here the same way. Concretely, measured: this
fix's own parent commit (`18d923a`, `docs/secdeploy-upstream.md` only -- touching neither a
control matrix nor `SHA256SUMS`) left every evidence-dependent field identical to ITS parent
(`0b435e2`, which is when this file's number was originally recorded as
`7d0a205333ced6c38f843aaa695b58ceb5da3be86172fa7aaf50491a8d4e8593`), yet regenerating at `18d923a`
with the pre-fix code produced a DIFFERENT `bundle_sha256`
(`d5df21cfca7ae4b9f3a3039f4a77c7fbd852800bc7c64344d4253a9673d36286`) purely because `git_commit`
had changed -- proof the old rule was broken, and exactly the failure this fix removes. With the
fix applied, regenerating at `18d923a` produces this file's current number
(`4d55fe50888b9df89cc64fb4b0f26f9fc0c8efd2e38aed01e4a26487cebfc32a`), and it will keep producing
that same number at any later commit that does not touch an evidence-dependent input.

Two regenerations at the literal SAME commit, with the same `--kit`/`--ledger-dir` state
(including "neither"), always reproduce the identical `bundle_sha256`
(`tests/test_evidence_bundle.py::test_two_runs_over_the_same_state_are_byte_identical`), and now
so do two regenerations at *different* commits that touch none of the evidence-dependent inputs
above (`tests/test_evidence_bundle.py::test_git_commit_is_excluded_from_what_bundle_sha256_hashes`).

## The live half (D4b, commit 2): `scripts/kit/live_evidence.py`

`scripts/kit/evidence.py` alone (the command above, no other flags) is the OFFLINE half of D4
only -- by itself, its `ledger_verify.live` and `secdeploy_evidence` sections are always the
declared, named `"not_collected"` placeholders above (never fabricated, never a silently-empty
dict that would read as "verified"). `scripts/kit/live_evidence.py` supplies both, for real:

```
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export GMAT_ROOT="/Users/probe/code/AltaVista/GMAT R2026a"
export CFS_MIRROR_DIR=/Users/probe/code/AltaVista/third_party/mirrors
.venv/bin/python scripts/kit/live_evidence.py --out out/evidence
```

**The deployment question (D4's own text), settled with evidence:** `secdeploy deploy macos`
cannot stand up any AltaVista component -- its `deploy()` function
(`/Users/probe/code/secdeploy/src/secdeploy/targets/macos.py`) is hand-written for a fixed set of
secdeploy-native component names only, with no generic per-manifest-component dispatch; measured
directly, `secdeploy deploy macos --dry-run` over our own merged manifest never mentions one of
our eight components
(`tests/test_live_evidence.py::test_deploy_macos_dry_run_never_mentions_an_altavista_component`).
So `live_evidence.py` stands up an evaluation placement we fully control instead -- our own
compose from the kit, the charter's sanctioned fallback -- never a real `secdeploy deploy` against
the user's own checkout.

**What it brings up, cheaply, for real:** `av-ingest-server` (docker, from the kit's own
cross-built binary and hash-verified image -- D4b's own floor), `av-dynamics-service` (a plain
native subprocess, real GMAT warm-up, no docker/cross-build needed), and `gmat-service`
(in-process, `gmat_service.admin.serve_admin` reused directly). Each contributes a REAL
`/admin/api/evidence/verify` response to `ledger_verify.live`. `av-command` (needs an OIDC
issuer/public key this task cannot supply), `av-edge-plugin` (a one-shot CLI client, no admin
server of its own), and `av-gateway` (its one admin route is a Bearer-token-gated evidence-bundle
AGGREGATOR, not a ledger `verify` endpoint, needing a live catalogue/proposer connection) are
recorded by name in `ledger_verify.live` with their own reason, never silently omitted -- see
`live_evidence.NOT_COLLECTED_COMPONENTS`.

**What `secdeploy evidence`/`secdeploy audit verify` actually contribute:** run for real over our
merged manifest (`deploy/secdeploy/merge.py`'s output), captured verbatim into
`secdeploy_evidence`. `secdeploy evidence`'s own `COMPONENTS` constant
(`secrouter`/`seccert`/`secllm`/`secchat`/`secrecorder`) can never name an AltaVista component
(`docs/secdeploy-upstream.md` Proposal 2) -- measured, every one of those five reports
`"skipped"` (DNS-unreachable; our eval site names them but nothing answers on this host) rather
than `"ok"`. Its real contribution here is its own deploy-audit chain verify result (`{"ok": true,
"checked": 0, ...}` -- no real deploy has run yet, so there is nothing to break) plus those five
named, non-fabricated per-component records. `secdeploy audit verify`, run separately and also
captured, reports the identical "no chained audit files found yet" result.

Both plug into `scripts/kit/evidence.py::assemble_bundle` verbatim, never re-fetched by that
module itself: `ledger_verify_live` (see `LIVE_LEDGER_VERIFY_NOT_COLLECTED`'s own `"plug_in"`
field) and `secdeploy_evidence` (see `SECDEPLOY_EVIDENCE_NOT_COLLECTED`'s own `"plug_in"` field).
Determinism holds across the live half too: live responses are INPUTS to `assemble_bundle`, and
two calls fed the same saved live-input dict produce byte-identical bytes
(`tests/test_live_evidence.py::test_two_bundles_over_the_same_saved_live_input_are_byte_identical`).
The one live field this task deliberately did not strip or normalise is `av-dynamics-service`'s
own `fips.detail` string (names this host's OpenSSL install path/version) -- real, reproducible on
THIS host, not a wall-clock value, and legitimately host-dependent (which is what makes it
evidence, not noise); it is recorded as-is.

The live tampered-ledger proof (D4b's own "if achievable offline, add it"): achievable, and added
-- `gmat_service.admin.serve_admin` (reused, not re-implemented) started over a REAL, deliberately
tampered `EvidenceLog`, its `/admin/api/evidence/verify` fetched over real loopback HTTP, reports
the break (`tests/test_live_evidence.py::
test_gmat_service_verify_over_tampered_ledger_reports_the_break_live`; `tests/test_compliance.py::
test_admin_evidence_verify_detects_a_tampered_record_via_http` already covers the identical
surface directly against `gmat_service.admin`, restated here through `live_evidence.py`'s own
collector).
