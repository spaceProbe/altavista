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
7d0a205333ced6c38f843aaa695b58ceb5da3be86172fa7aaf50491a8d4e8593
```

Measured by running the command above with no `--kit`/`--ledger-dir` (both offline-declared
slots) and reading `bundle_sha256` from the written `out/evidence/bundle.json` -- the same value
the command's own stderr prints. **This number will not reproduce on a different commit** unless
the epoch inputs below are unchanged since this commit -- see "What the hash depends on".

## What the hash depends on (the epoch rule)

Same discipline `docs/compliance/sbom/README.md`'s own "Determinism" section already carries for
the ten committed SBOMs, applied here to the bundle:

- **The six control matrices** (`docs/compliance/{av-command,av-dynamics-service,av-edge-plugin,
  av-gateway,av-ingest,gmat-service}/control-matrix.md`) -- every row, every deficiency, and each
  file's own SHA-256 (which the bundle also records per-component, so a reader can independently
  confirm which exact text a `control_matrices` entry summarised).
- **`docs/compliance/sbom/SHA256SUMS`** and, transitively, the ten `*.cdx.json` files it records
  hashes for (the bundle re-verifies every one against the files on disk -- a stale `SHA256SUMS`
  becomes a recorded `sbom_hashes.stale` finding, not a silent pass-through of the recorded
  value).
- **Whatever `--kit`/`--ledger-dir` content the invocation was given** (or, given neither, the
  fixed "not given this run" placeholders) -- these are run-time inputs, not committed files, so
  they do not move with a commit the way the two bullets above do; two regenerations at the SAME
  commit with the SAME `--kit`/`--ledger-dir` state (including "neither") produce the identical
  hash (`tests/test_evidence_bundle.py::test_two_runs_over_the_same_state_are_byte_identical`).
- **`git rev-parse HEAD`** (`git_commit`) -- recorded for provenance (which commit this bundle
  was built against), and, being part of the bundle's own canonical content like every other
  field, part of what `bundle_sha256` covers too (deliverable 7 excludes only the hash field
  itself from what it hashes -- nothing else).
- **`epoch`**, a git-derived timestamp -- never `datetime.now()`; `git log`'s own committer date
  for exactly the paths in the first two bullets (`bundle.json`'s own `epoch_paths` field lists
  them, so a reader never has to re-derive which commits touch them, matching the SBOM README's
  own transparency rule).

Two consequences worth separating, since they answer different questions:

- **Does `bundle_sha256` change between two commits?** Yes, always -- `git_commit` changes with
  every commit, and it is hashed like every other field. `bundle_sha256` is not itself a "did the
  evidence change" signal across commits.
- **Did the evidence a reader would actually care about change?** Compare `epoch` instead (or,
  more precisely, each `control_matrices.<component>.sha256` / `sbom_hashes.actual` entry): those
  only move when a commit actually touches `epoch_paths` -- a control matrix or a committed SBOM.
  A commit that touches neither leaves `epoch` and every content hash identical even though
  `bundle_sha256` itself will still differ (because `git_commit` did). This is why the bundle
  records both a content-derived `epoch` AND a commit-derived `git_commit`/`bundle_sha256`,
  rather than treating the top-level hash alone as the "did anything change" answer.

Two regenerations at the literal SAME commit, with the same `--kit`/`--ledger-dir` state
(including "neither"), always reproduce the identical `bundle_sha256`
(`tests/test_evidence_bundle.py::test_two_runs_over_the_same_state_are_byte_identical`).

## What is NOT collected by this half

`scripts/kit/evidence.py` is the OFFLINE half of D4 only. Its `ledger_verify.live` section is
always a declared, named `"not_collected"` placeholder (never fabricated, never a silently-empty
dict that would read as "verified") until a second worker's live collection --
`secdeploy evidence` run over an actually-reachable deployed placement, plus real
`/admin/api/evidence/verify` results fetched from each running component's own HTTP endpoint --
supplies it. `scripts/kit/evidence.py:assemble_bundle`'s `ledger_verify_live` parameter is the
exact, documented plug-in point (see that function's own doc, and
`LIVE_LEDGER_VERIFY_NOT_COLLECTED`'s own `"plug_in"` field, which carries the same instructions
into every bundle that has not yet been given a live half).
