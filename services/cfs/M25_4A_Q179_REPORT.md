# M25.4a part 3 -- question 179: cFS image digest pin (worker report)

Scope: `services/cfs/` only (build script, test, digest doc, manifest). No `crates/**`
touched. Measurements recorded here as they were taken, in order.

## 1. Reading the decision (docs/open-questions.md question 179 + its 2026-09-07 amendment)

Question 179 text (verbatim location: `docs/open-questions.md` line 320):

> The cFS image digest pin drifted with an untouched source tree, and the test builds the
> image at test time (team 2, round 2): the rebuild is deterministic (same digest twice) but
> differs from the recorded pin, so something in the build context changed without a recorded
> reason; and building at test time fetches over the network, which question 154 allows only
> as a one-time build step. Decided by the lead: the image is built by a documented one-time
> script (`services/cfs/build-image.sh`) that also writes a build-context manifest (every
> COPYed path with its SHA-256) beside `IMAGE_DIGEST.md`; the digest test only inspects an
> already-built image and skips visibly when none exists; a digest mismatch reports which
> manifest entries changed, so drift is attributable; the current drift is re-pinned once with
> the manifest recorded and its cause named from the manifest diff. M25.4a.

Amendment (2026-09-07, commit `1151664`, added to the same question): a fresh clone passes 434
Python tests and fails only the image-digest test; the Dockerfile COPYs
`services/cfs/bin/av-lockstep-shim`, a compiled cross-build artifact deliberately not tracked
in git (`.gitignore` line 35). Checked: that binary already exists on this host
(`services/cfs/bin/av-lockstep-shim`, 2,303,928 bytes, mtime 2026-09-06 01:03) so no cargo
build is needed here -- the file is present, this task only has to hash it into the manifest.
(The task brief for this worker also says explicitly: do not run cargo.)

## 2. Dockerfile COPY inventory (verified against the file directly, not against the brief)

`grep -n "^COPY" services/cfs/Dockerfile` (repo-root-relative host source path -> image dest):

1. `third_party/fetch-cfs.sh` -> `third_party/fetch-cfs.sh`
2. `services/cfs/apps/io_lockstep` -> `third_party/cfs/apps/io_lockstep`
3. `services/cfs/apps/sch_lockstep` -> `third_party/cfs/apps/sch_lockstep`
4. `services/cfs/apps/adcs` -> `third_party/cfs/apps/adcs`
5. `services/cfs/apps/shared` -> `third_party/cfs/apps/shared`
6. `services/cfs/psp-lockstep` -> `third_party/cfs/psp-lockstep`
7. `services/cfs/build/targets.cmake` -> `third_party/cfs/sample_defs/targets.cmake`
8. `services/cfs/build/generate_startup.cmake` -> `third_party/cfs/sample_defs/generate_startup.cmake`
9. `services/cfs/build/cpu1_install_custom.cmake` -> `third_party/cfs/sample_defs/cpu1/install_custom.cmake`
10. `services/cfs/bin/av-lockstep-shim` -> `/cfs/av-lockstep-shim`
11. `services/cfs/container-entrypoint.sh` -> `/cfs/container-entrypoint.sh`

Excluded: `COPY --from=builder /build/third_party/cfs/build-native_std/exe/cpu1 /cfs/cpu1`
(Dockerfile line 89) -- this is an intra-image copy from the `builder` stage, not a host path,
so it cannot appear in a host-path manifest and is deliberately not in the list above. This
matches the background brief's list exactly (fetch-cfs.sh; apps/{io_lockstep,sch_lockstep,
adcs,shared}; psp-lockstep; build/{targets.cmake,generate_startup.cmake,
cpu1_install_custom.cmake}; bin/av-lockstep-shim; container-entrypoint.sh) -- 11 confirmed here
because the background list groups the 4 `apps/*` entries and the 3 `build/*.cmake` entries.

## 3. Pre-build filesystem audit (host state before touching anything)

Goal: find out, from file mtimes and git history alone, whether any COPYed host path has
changed content since `IMAGE_DIGEST.md`'s recorded pin (`sha256:27ed4ff8...`, file mtime
2026-09-06 17:35:11 local).

- `git log --oneline -- services/cfs/` -> only two commits touch this tree in this repo's
  history: `474b76a` (Initial import of the Alta Vista platform) and `8191ead` (Fix packaging
  for a clean install and un-hide files the build/ ignore swallowed, 2026-09-07 23:00, i.e.
  AFTER the recorded pin's mtime).
- `8191ead`'s diff (`git show --stat 8191ead -- services/cfs/`) adds 8 files under
  `services/cfs/build/` as newly tracked, including 3 of our 9 host COPY paths
  (`cpu1_install_custom.cmake`, `generate_startup.cmake`, `targets.cmake`). Root cause per the
  commit message: the repo's `.gitignore` had an unanchored `build/` pattern that also matched
  `services/cfs/build/` (only meant to ignore a top-level `/build/`), so these files were
  gitignored and were never in the initial import. `8191ead` anchors the pattern to `/build/`
  and adds the files.
- Critically, this is a **git-tracking** fix, not a content edit: `ls -la` on those 3 files
  shows mtimes of 2026-09-05 23:57 / 2026-09-06 00:59 / 2026-09-06 00:59 -- all *before* the
  recorded-pin mtime (2026-09-06 17:35) and untouched since (this is the same working tree the
  pin was recorded on, not a fresh clone). So the bytes `8191ead` committed are the same bytes
  that were already on disk when `27ed4ff8...` was built; git only started tracking them later.
  Hypothesis: **this explains why a fresh clone before `8191ead` could not build the image at
  all (COPY of a path git never checked out), but it does NOT by itself change the image's
  content-addressed ID on this host**, because the file bytes are unchanged.
- Checked every other COPY path the same way: `find services/cfs/apps services/cfs/psp-lockstep
  -newer services/cfs/IMAGE_DIGEST.md` returns 0 files (nothing under apps/ or psp-lockstep/ is
  newer than the recorded pin). `services/cfs/bin/av-lockstep-shim` mtime 2026-09-06 01:03 (older
  than the pin). `services/cfs/container-entrypoint.sh` mtime 2026-09-06 02:00 (older than the
  pin). `third_party/fetch-cfs.sh` mtime 2026-09-06 16:25 (older than the pin, and unchanged in
  git since the initial import per `git log -1 -- third_party/fetch-cfs.sh` = `474b76a`).

**Hypothesis, stated before building:** every host COPY path's content is unchanged since the
recorded pin was written (verified above by mtime + git history), so I expect the
build-context manifest this task produces to show **zero changed/added/removed entries**
relative to the pin. If the freshly built digest nonetheless differs from `27ed4ff8...`, the
cause is outside the COPY set entirely. Named candidates to check in that case, in order of
likelihood: (a) `FROM ubuntu:22.04` is a floating tag -- Docker resolves it to whatever
`ubuntu:22.04` points to today, which Canonical updates periodically, changing every layer
after `FROM` even with identical COPY inputs; (b) the two `apt-get install` lines pin no
package versions, so a repo-side package update changes installed bytes; (c)
`third_party/fetch-cfs.sh`'s clone is pinned by commit hash (not just tag), so it should be
stable content, but is worth checking with `--build-arg`/BuildKit-cache-busting if (a)/(b) are
ruled out; (d) local Docker/BuildKit version drift changing metadata embedded in `.Id`.

(Measurements from the actual build appended below once the contention gate clears and the
build has run.)

## 4. Built artifacts (design, before the real build)

- `services/cfs/build-image.sh`: parses `COPY` lines out of the live Dockerfile (skips
  `--from=` lines), so the manifest's file list cannot drift out of sync with the Dockerfile in
  a second hardcoded place. Verified the parser against the real Dockerfile with a standalone
  dry run (see below) before wiring it into the script: it produces exactly the 10 host source
  paths (9 non-`--from=` COPY lines... actually 10, see next line) expected, correctly skipping
  the one `--from=builder` line.

  Parser dry-run output (`grep -E '^COPY[[:space:]]'` + the same tokenizing the script uses),
  run against `services/cfs/Dockerfile`:
  ```
  SRC=third_party/fetch-cfs.sh
  SRC=services/cfs/apps/io_lockstep
  SRC=services/cfs/apps/sch_lockstep
  SRC=services/cfs/apps/adcs
  SRC=services/cfs/apps/shared
  SRC=services/cfs/psp-lockstep
  SRC=services/cfs/build/targets.cmake
  SRC=services/cfs/build/generate_startup.cmake
  SRC=services/cfs/build/cpu1_install_custom.cmake
  SKIP(from): COPY --from=builder /build/third_party/cfs/build-native_std/exe/cpu1 /cfs/cpu1
  SRC=services/cfs/bin/av-lockstep-shim
  SRC=services/cfs/container-entrypoint.sh
  ```
  10 host source paths, 1 skipped `--from=` line. Matches the Dockerfile read manually in
  section 2 above.

  A dry run of the hashing/expansion logic (same directory-recursion the script uses) against
  the current tree produced **38 file entries** (4 `apps/*` directories expand to 30 files
  total, plus 3 `build/*.cmake` files, 1 `bin/av-lockstep-shim`, 1 `container-entrypoint.sh`, 1
  `psp-lockstep` directory expanding to 2 files, 1 `third_party/fetch-cfs.sh`). Cross-checked:
  `find services/cfs/apps services/cfs/psp-lockstep -type f | wc -l` = 32 (30 apps/* + 2
  psp-lockstep files), no stray non-source files (`.DS_Store` etc.) present under either tree.

  Chose dynamic parsing over a hardcoded list (per the task's own fallback option) because the
  Dockerfile's COPY lines are all in the simple `COPY <src> <dst>` / `COPY --from=X <src> <dst>`
  shape and a hardcoded list is exactly the second-place-to-drift the task brief warns about.
  The parser fails loudly (`die`) on any COPY line shape it doesn't recognize (an unexpected
  flag, or not exactly 2 positional tokens), so a future Dockerfile edit that breaks the
  parser's assumption is a hard script failure, not a silently wrong manifest.

- `services/cfs/tests/test_image_digest.py`: two tests --
  `test_manifest_paths_exist_and_hash_match` (pure file-hash check, not Docker-gated, runs
  everywhere) and `test_image_digest_matches_recorded_value` (Docker-gated: skips with a
  reason naming `services/cfs/build-image.sh` when Docker is unavailable or the image tag
  isn't built; on a digest mismatch, re-derives the current COPY-set manifest independently
  from the live Dockerfile -- not by re-reading `IMAGE_CONTEXT_MANIFEST.txt`'s own file list --
  and diffs it against the recorded manifest, printing added/removed/changed paths, or the
  explicit "nothing in the build context moved, look outside it" message if the diff is empty).

---

## Manager completion (2026-09-08)

The worker built every artifact above but stopped before the real build, the re-pin and the
gates (it ended its turn waiting for the contention gate rather than blocking on it -- question
157's pattern again, and the manager's fault for briefing "wait" without giving a blocking
command the first time). Everything below is the manager's own.

### 5. The real build, and the hypothesis outcome

`services/cfs/build-image.sh` run for real on a quiet host. It parsed 11 host COPY paths,
skipped the one `--from=builder` line, and wrote a 38-entry manifest.

- New image ID: `sha256:29eb1bec64b7e826de157468c79dd82103b80d42b7debc5fa809727dea994cde`
- Previous recorded pin: `sha256:27ed4ff89dee381258a9ccb8410fa8aae19312defc79bf815ddc2e2c509d21ae`

**The worker's hypothesis held**: the build-context diff is EMPTY. `git status` reports no
modification to any COPYed path, and the freshly written manifest matches the tree exactly
(proven by the deliberate break below, whose failure message is the empty-diff branch).

**The cause, named from the artifact rather than from the candidate list.** The worker's leading
candidate was the floating `FROM ubuntu:22.04` tag. That is excluded: the local `ubuntu:22.04`
resolves to `sha256:2edbbc5dc405...`, created 2026-08-10, i.e. before the previous pin, and the
build log shows the second-stage `apt-get` layer served from cache while the builder stage
genuinely re-ran. The real cause is one layer deeper and is stated by upstream itself --
`third_party/cfs/cfe/cmake/generate_build_env.cmake:15-23` sets `BUILDDATE` from `date
+%Y%m%d%H%M` when `$BUILDDATE` is unset and links it into cFE's `CONFIGDATA`, with the comment
"may be passed via environment variables to force a particular date ... if hoping to reproduce
an exact binary of a prior build". `services/cfs/Dockerfile` sets neither `BUILDDATE` nor
`BUILDUSER`/`BUILDHOST`/`SOURCE_DATE_EPOCH` (grepped). **So this image cannot be reproducible:
any build that actually re-executes the builder stage stamps the current minute into `cpu1`.**
That also explains question 179's "deterministic (same digest twice)" observation -- two
rebuilds agree when they land in the same minute or are served whole from the layer cache.

Full write-up and the recommended fix (fix those three variables in the Dockerfile, rebuild
once, pin that -- a lead decision, since it changes the image) are in
`services/cfs/IMAGE_DIGEST.md`'s "Re-pinned 2026-09-08" section.

The empty-diff branch of the test's own message was extended by the manager to name this cause,
so the next person to hit it is told the likely answer instead of only a candidate list.

### 6. Break-and-restore (manager's own)

**`test_image_digest_matches_recorded_value`** -- wrong implementation: a recorded digest that
does not match the built image (first 8 hex digits of the pin changed to zeros). This is also
the only way to exercise the empty-diff branch, which is the new behaviour question 179 asked
for:

```
E  Failed: built image digest 'sha256:29eb1bec...' does not match .../IMAGE_DIGEST.md's
   recorded 'sha256:00000000...'.
E  manifest and current build context agree EXACTLY (no path added, removed, or changed) --
   whatever moved the digest is NOT in the COPYed build context; ...
1 failed, 1 passed in 0.13s
```

**`test_manifest_paths_exist_and_hash_match`** -- wrong implementation: a corrupted hash in the
manifest (first 8 hex digits of one entry replaced). Proves the check is per-entry and
attributable, not a count:

```
E  Failed: .../IMAGE_CONTEXT_MANIFEST.txt is stale (0 missing, 1 changed):
E    ~ changed: services/cfs/apps/adcs/CMakeLists.txt  (recorded sha256:deadbeef939fe35d...
     -> actual sha256:cc102cc6939fe35d...)
E  Re-run `services/cfs/build-image.sh` to regenerate the manifest for the current tree.
1 failed, 1 passed in 0.13s
```

Both restored from byte-exact backups; the suite is green again (below).

### 7. Gates (manager's own, isolated, host confirmed quiet before each)

- `.venv/bin/python -m pytest -q services/cfs/tests/ -rs`: **19 passed**, 0 failed, **0
  skipped** -- Docker is available and the image is built on this host, so the Docker-gated
  digest test genuinely ran rather than skipping.
- `.venv/bin/python -m pytest -q` (full suite): **436 passed**, 0 failed, 0 skipped, in 207 s.
  Baseline was 434 passed **plus one failing image-digest test**. The arithmetic: that failure
  is now a pass (+1 passing) and the old single test became two (+1 test), so 434 + 1 + 1 = 436.
  Nothing else changed state. **The one known failure in this repository's Python suite is
  closed.**
- No cargo run (out of scope for this part).

### 8. For the lead

`BUILDDATE`/`BUILDUSER`/`BUILDHOST` are not pinned, so this digest will drift again on any
cache-evicted rebuild. Pinning them makes the image reproducible and the pin durable, but it
changes the image and therefore the digest once more. Recommended, not done.
