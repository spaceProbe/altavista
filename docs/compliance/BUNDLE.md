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
e116b9822f73a8a6d2a5efac37c139895fb82760a9330b0607754c1ec5e88855
```

Measured by running the command above with no `--kit`/`--ledger-dir` (both offline-declared
slots) and reading `bundle_sha256` from the written `out/evidence/bundle.json` -- the same value
the command's own stderr prints.
`tests/test_evidence_bundle.py::test_bundle_sha256_matches_the_hash_recorded_in_bundle_md`
regenerates the bundle from the current tree and asserts its `bundle_sha256` equals this exact
number, parsed out of this file for real -- so this number is load-bearing, not decorative: if it
ever drifts from what the current tree actually regenerates, CI fails until this file is updated.

### Regeneration history

Newest first, one row per hash this file has ever RECORDED (in its own fenced block, at some
commit) -- every value, including the ones superseded within minutes. `scripts/kit/
regenerate_compliance.py` (round 3, question 227) maintains both the fenced block above and this
table's top row together, in the second of its own two commits -- see that script's own doc
comment for the full two-commit ordering rule. "Commit" names the commit whose regeneration (or
other evidence-changing content) this row's hash reflects; where that is a DIFFERENT commit from
the one that actually wrote the value into this file's fenced block, the "Cause" column says so
explicitly (the `d3f18b5`/`9cf8921` pair below is exactly this case -- read together). Where the
prior prose named no commit for a value, this table says so rather than inventing one. The fuller
per-move narrative this table condenses (exact `git show --stat` confirmations, byte-for-byte
re-generation proofs) lives in this file's own git history, `git log -p -- docs/compliance/
BUNDLE.md`. One further hash is named elsewhere in this document but deliberately has NO row
here: `d5df21cfca7ae4b9f3a3039f4a77c7fbd852800bc7c64344d4253a9673d36286` was never a value this
fenced block actually held -- it exists only as "What the hash depends on"'s own counter-example
measurement (what the pre-fix code would have produced at commit `18d923a`, alongside the real
value that commit actually recorded, `4d55fe50...`, the table's own oldest-but-one row below) --
so it is not duplicated here.

| Date | Hash | Commit | Cause |
|---|---|---|---|
| 2026-09-18 | `e116b9822f73a8a6d2a5efac37c139895fb82760a9330b0607754c1ec5e88855` | `bceec44` | scripts/kit/regenerate_compliance.py: `bundle_sha256` moved with no SBOM byte changed -- some other evidence-dependent input (e.g. a control matrix) changed; see this commit's own diff. |
| 2026-09-18 | `ae0dd61a355bdd72e698f27a5aeff1c71401dee144148ff9413a6efa02b3f6aa` | `2aced3e` | Regenerated the two Python SBOMs so their epoch reflects the newly-committed `python-lock.json` (the SBOMs had briefly predated that file, so their epoch was stale on landing). |
| 2026-09-17 | `2c306b76b89bd1701af0b542843873ebe866ac25813f9020de62777c6a4e0a32` | `6fe41d7` | Question 224 landed: the two Python SBOMs now source from the committed `python-lock.json`, not the live venv (dropped `pip`, updated the `python-packages-source` property). |
| 2026-09-16 | `459c107c164fc0756cb95c9a5a03659731a3296075e0c60abd45d2b63574d808` | `7ea2d24` | SBOMs regenerated for the heavy-round-2-into-native-dynamics merge's workspace-manifest epoch move (question 220). |
| 2026-09-16 | `06a1838aed06080ed56a8d9d46a5b131b02c3bbc74f5c3232fb24205e019db3e` | `d3f18b5` (correctly recorded by the following commit, `9cf8921`) | Two Python SBOMs regenerated from a complete venv (`pip install -e ".[dev]"`) by `d3f18b5`, correcting the row below's venv-incomplete hash -- but `d3f18b5` itself recorded that row's STALE value, not this one; `9cf8921` re-ran the documented command against the now-existing `d3f18b5` and recorded what it actually printed, this hash. |
| 2026-09-16 | `b5b99cac4338633f3ea960deecf7d4896b37bb438323632f1b1b07d4263fad1e` | `d3f18b5` | The stale-on-landing value: `d3f18b5` computed and recorded this in the SAME commit as its own SBOM regeneration, before the epoch-affecting commit it was itself part of actually existed -- wrong the instant it landed, per `scripts/kit/regenerate_compliance.py`'s own doc comment (this is the real historical proof it cites for the two-commit ordering rule). Corrected one commit later by `9cf8921` (row above), which re-ran the same command against `d3f18b5` and recorded `06a1838a...` instead. |
| 2026-09-16 | `c2670059ce354b9233a1e01455bead81c3223c0d919338f6321a8b0f2b899aec` | `53381cf` | Heavy-round-2 merge reconciliation recorded a hash computed from that incomplete venv -- wrong, corrected by the `06a1838a...` row above (question 224's own finding). |
| 2026-09-16 | `19992e76e3938671ae444e917067b713a5beac295e91de0ec1f571ec274c78d6` | `8c5d93a` | Same-day SBOM refresh; the commit that produced this exact value is not named in the prior prose. Superseded hours later when the aiplane merge's own SBOM regeneration (`4f559a9`) dropped stale distribution entries from `av-viewer.cdx.json`/`gmat-service.cdx.json`. |
| 2026-09-15 | `4d55fe50888b9df89cc64fb4b0f26f9fc0c8efd2e38aed01e4a26487cebfc32a` | `3f53aca` | Round 3 defect fix: excluded `git_commit` from `bundle_sha256`'s hashed content (a commit-inclusive hash was stale the instant it landed -- question 214's platform lesson). The illustration of this exact fix, with its own before/after hashes (including the pre-fix counter-example `d5df21cf...` named above) at commit `18d923a`, stays in "What the hash depends on" below. |
| 2026-09-15 | `7d0a205333ced6c38f843aaa695b58ceb5da3be86172fa7aaf50491a8d4e8593` | `0b435e2` | This file's FIRST recorded value, from D4a ("the offline half of the evidence bundle"), the commit that first assembled and recorded the bundle at all. Superseded by the row above once the round-3 `git_commit`-exclusion fix landed. |

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
