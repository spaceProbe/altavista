# M26.1 Rename: gmatviz -> altavista

status: complete

Task: repo-wide rename of `gmatviz` to `altavista` (package, CLI, server, tests, docs,
launch config, viewer title/HUD "Alta Vista", hashed state-space ids). No alias for the old
name. Per docs/open-questions.md Q160-163 and docs/ui-rework-plan.md M26.1.

## Not done / running list (updated as I go)

- [x] Survey scope (measured counts match the brief's numbers exactly: gmatviz/ 44, web/ 20,
      tests/ 25, crates/ 18, drms/ 4, goldens/ 11, docs/ 11, services/ 19, proto/ 2 (leave),
      third_party/ 2 (leave), .claude/ 1. Also found: examples/ (9 hits, 5 files, not in the
      brief's count but clearly in scope), README.md, pyproject.toml, .dockerignore,
      gmatviz.egg-info/ and .venv site-packages (build artifacts, handled by reinstall).)
- [x] Rename gmatviz/ -> altavista/ package dir (`mv gmatviz altavista`)
- [x] Update pyproject.toml (name, [project.scripts] entry point, packages.find include).
      testpaths unaffected (already `tests`, `services/cfs/tests`, no gmatviz-named path).
- [x] Update web/ (viewer identifiers + display string "Alta Vista")
- [x] Update tests/
- [x] Update crates/
- [x] Update drms/ (comments only, see hash table -- no DRM/system hash changed)
- [x] Update goldens/ (4 affected goldens regenerated with --reason, hash table below;
      confirmed no other golden mentions gmatviz)
- [x] Update docs/ (excluding proto comments, docs/adr/**, docs/teamlog/**; see justification
      list for what was deliberately left)
- [x] Update services/
- [x] Update .claude/launch.json
- [x] On-disk paths / GMAT log file name -- checked, nothing found that embeds "gmatviz" in a
      path or log filename beyond the package directory itself and the Python logger name
      (both handled). GmatLog.txt is GMAT's own fixed log filename, unrelated to this rename.
- [x] Confirm physics unchanged: exact `|dr| = 0.0108 m` figure the brief cites, reproduced
      verbatim via `--nocapture` (see "Physics unchanged" section)
- [x] Run gates: cargo build (green), cargo test --workspace --exclude av-kernel (green),
      cargo test -p av-kernel (**665 passed, 0 failed** -- exact baseline match), cargo clippy
      -D warnings (green), cargo deny check (green), .venv/bin/pytest -q (354 passed + 1
      pre-existing unrelated failure = 355 total -- see "Gate output")
- [x] grep -rn gmatviz final sweep, justify every remaining hit (see "Remaining gmatviz hits")
- [x] Drive demo end to end (av-run -> POST /api/cdm/run -> viewer), both system yamls,
      confirmed identical config hash and correct render (see "Demo drive result")
- [x] Final report sections filled in, all numbers confirmed (no placeholders remaining)

## SIGNIFICANT FINDING: protobuf Python-binding namespace collision (found and fixed)

The mechanical rename directly caused a real bug, not just a text change. Before the rename,
the Python package was `gmatviz`, and its committed protobuf bindings live at
`gmatviz/pb/altavista/v1/*.py` (nested `altavista/v1` because the *proto* package has been
named `altavista.v1` since question 3's original 2026-09-02 answer -- unrelated to this
task). protoc's codegen makes cross-file imports absolute and rooted at the proto package
path (`from altavista.v1 import core_pb2 as ...`). Since that never matched the containing
Python package's name (`gmatviz`), `altavista/pb/__init__.py` (then `gmatviz/pb/__init__.py`)
put its own directory on `sys.path` once at import time so the bare top-level name
`altavista` -- distinct from `gmatviz` -- would resolve to the nested proto bindings.

Renaming the top-level package to `altavista` (as directed) makes it collide with that
already-existing nested `altavista` proto-bindings package: by the time `altavista/pb/__init__.py`
runs (reached from `altavista/__init__.py` -> `frames.py` -> `.pb`), `sys.modules['altavista']`
is already bound to the real top-level package (partially initialized), so the `sys.path`
entry is never consulted; `from altavista.v1 import core_pb2` resolves `altavista.v1` against
the real package's own `__path__` (no `v1` there) and raises
`ModuleNotFoundError: No module named 'altavista.v1'`. Reproduced directly: `import altavista`
failed with exactly this traceback immediately after the directory rename + reinstall.

Fix (in `altavista/pb/`, not `proto/` -- the proto package name `altavista.v1` itself is
unchanged and correct): rewrote every generated cross-import from the absolute
`from altavista.v1 import X as Y` to a plain relative `from . import X as Y` (17 lines across
10 `*_pb2.py`/`*_pb2_grpc.py` files), and `altavista/pb/__init__.py` to reach them with a
relative `from .altavista.v1 import (...)` instead of the sys.path hack. Made this durable,
not a one-off hand patch: added `_rewrite_cross_imports_relative()` to
`altavista/pb/generate.py`, called on every generated `*_pb2.py` (right after the `protoc`
subprocess) and every kept `*_pb2_grpc.py` (in `_generate_grpc_stubs`), and updated
`PB_INIT_CONTENT` (the template `generate.py` writes to `pb/__init__.py`) and the module
docstring to match. Verified by actually re-running `.venv/bin/python altavista/pb/generate.py`
(protoc `libprotoc 35.1`, grpcio-tools `1.83.1` -- both match the versions pinned in
generate.py's own `PROTOC_VERSION`/`GRPC_TOOLS_VERSION`) against a backed-up copy: output is
byte-identical except `pb/__init__.py`'s comment length (the hand-patch had a longer inline
explanation; the regenerated template points to generate.py's own docstring instead, which
carries the same explanation). This proves the fix is deterministic and reproducible from the
generator, not something that only works by accident of my manual edit order.
`import altavista`, `python -m altavista --help` and `.venv/bin/altavista --help` all verified
working after the real regeneration.

This is a structural consequence of choosing "altavista" as the new package name when a
same-named proto-bindings subpackage already existed nested inside it -- flagged explicitly
per the standing instruction to investigate oddities rather than wave them through.

### Second occurrence of the same collision, found by the pytest gate

`.venv/bin/pytest -q` initially reported **3 errors** (not the expected 355 passed), all in
`tests/test_cdm_v1.py`: the exact same `ModuleNotFoundError: No module named 'altavista.v1'`.
This file has its **own**, separate ad-hoc protoc invocation (compiles `proto/altavista/v1`
fresh into `build/pb` on every test run, specifically to catch drift between `proto/` and the
committed `altavista/pb/` output -- see its module docstring), so fixing `altavista/pb/`
alone did not fix it; it needed its own fix. Root cause identical to the one above. Fix
(`tests/test_cdm_v1.py`): (1) rewrite the freshly-generated absolute cross-imports to
relative, same regex as `generate.py`; (2) load the seven `*_pb2` modules under a private
alias package (`_test_cdm_v1_pb.v1`, hand-built `types.ModuleType` namespace registered in
`sys.modules` with the right `__path__`) instead of the real `altavista.v1` name, so this
fixture never imports a bare top-level `altavista` at all -- no collision possible regardless
of what the real package is named. Verified: `pytest -q tests/test_cdm_v1.py` -> `3 passed`
(previously 3 errors).

This means the "altavista"/proto-namespace collision had (at least) two independent
manifestations in this codebase -- the committed bindings and this one ad-hoc test generator
-- both now fixed by the same technique. Swept for any *other* occurrence: every file doing
`sys.path.insert`/`sys.path.append` anywhere under `altavista/`, `tests/`, `web/`,
`services/`, `drms/`, `crates/`, `examples/` was checked by hand for the dangerous pattern
(inserting a directory that itself contains a nested `altavista/v1` tree, then bare-importing
`altavista.v1...`/`importlib.import_module("altavista...")`). Every other occurrence either
inserts a directory so the top-level `altavista` package itself resolves (no collision -- that
*is* the real package) or reaches the proto bindings through the real package's own hierarchy
(`from altavista.pb.altavista.v1 import ...`, e.g. `tests/lockstep_local_peer.py`,
`tests/test_lockstep_ref.py` -- the safe pattern this task's own fix established). No third
occurrence found.

### Minor polish found by re-reading the diff (not bugs, but worth recording)

- **Article agreement**: the bulk substitution is a literal string swap, so every occurrence
  of "a gmatviz ..." (correct: "gmatviz" starts with a consonant sound) mechanically became
  "a altavista ..." (wrong: "altavista" starts with a vowel sound). Swept for this
  specifically (`\ba altavista\b`, `\bA altavista\b`, and the backtick-quoted form
  `` a `altavista` ``) and fixed 9 files: `README.md`, `altavista/FRAMES.md`,
  `altavista/CDM.md`, `altavista/cdm.py`, `altavista/model.py`, `tests/test_cdm_adapter.py`,
  `crates/av-kernel/tests/state_space_declaration.rs`, `drms/README.md`,
  `docs/architecture.md`. Re-checked after: zero remaining `a altavista`/`A altavista`
  anywhere in the tree.
- **`altavista/bodies.py:66`** constructs a GMAT `CoordinateSystem` object named
  `f"altavista_{body}Fixed"` (was `f"gmatviz_{body}Fixed"`) -- an embedded identifier string,
  not a comment, so it needed the same scrutiny as the three hashed state-space ids. Verified
  safe: this name is a purely internal, per-process cache key / GMAT object handle (checked
  against `g.Exists()` and a local `_cs_cache` dict), never serialized to JSON/proto output
  and never referenced by any test, golden, or other module by that literal string (grepped
  the whole tree for `body_fixed_system`/the pattern itself -- only its own definition and
  one caller). Renaming it changes nothing observable.

### Docker image digest test failures -- investigated, one is a transient timing issue (now
### passing), one is pre-existing and unrelated to this task

`.venv/bin/pytest -q` also reported 2 failures, both Docker image builds:

1. **`tests/test_lockstep_ref.py::test_docker_image_lifecycle_pull_by_digest_run_bind_and_remove`**
   -- timed out at its hardcoded 120 s build timeout. `services/lockstep-ref/Dockerfile` does
   `COPY altavista/pb /app/altavista/pb` and `COPY services/lockstep-ref/lockstep_ref
   /app/lockstep_ref`, both of which this rename legitimately changed (the directory rename
   and the text substitution respectively), so Docker's build cache was correctly invalidated
   and a real rebuild (pip install of grpcio/protobuf, "Sending build context to Docker daemon
   4.319GB") was needed -- not by itself a bug. Investigated by building the same Dockerfile
   by hand: succeeded in well under a minute once the earlier `cargo test`/`cargo build` load
   had cleared. Re-ran just this test afterward: **passed** (`1 passed, 12 deselected in
   29.64s`). Concluded: transient resource contention (this pytest run, the concurrent `cargo
   test --workspace --exclude av-kernel` background job, and the M24 container build the task
   brief says is running on this host, all competing for CPU/disk at once), not a functional
   regression. The manually-built check image (`lockstep-ref:m26-check`) was removed after
   inspection so no stray tag was left behind.
2. **`services/cfs/tests/test_image_digest.py::test_image_builds_and_digest_matches_recorded_value`**
   -- fails on a **pre-existing** digest mismatch, unrelated to this task. Investigated:
   `services/cfs/Dockerfile` only `COPY`s `third_party/fetch-cfs.sh`, `services/cfs/apps/**`,
   `services/cfs/psp-lockstep`, three `services/cfs/build/*.cmake` files, the prebuilt
   `services/cfs/bin/av-lockstep-shim` binary, and `services/cfs/container-entrypoint.sh` --
   **none of which this rename touched** (`services/cfs/` had zero `gmatviz` references to
   begin with, and `third_party/` is out of scope by the task brief). `docker images` showed
   the `altavista-cfs-lockstep:local` image **already present in the local Docker daemon
   before any of this session's edits**, with `Id` `sha256:3bae6444...` -- already disagreeing
   with `services/cfs/IMAGE_DIGEST.md`'s recorded `sha256:abe1631c...`. Re-running the test in
   isolation reproduces the same mismatch in 21.69 s (cache-hit fast, not a rebuild), which
   rules out a timing/rebuild explanation and confirms it is a genuinely different image than
   the one the digest doc records -- most likely drift from the M24 container/toolchain work
   the brief says is running concurrently on this host (`third_party/rtems*`, `third_party/renode/`,
   and by extension `third_party/cfs`, are its evidence and explicitly off limits here). Left
   untouched, as instructed (`third_party/` and anything feeding the M24 evidence is out of
   scope); flagged here for the lead rather than "fixed" by rebuilding or editing
   `IMAGE_DIGEST.md`, since that recorded value is not this task's to change.

## Hash change table

These are the physics reference goldens under `goldens/` whose `note`/`reason` text
mentioned `gmatviz` (module-path prose, e.g. "gmatviz.scenario.Scenario.maneuver's own VNB
path"). Each golden's own `sha256` field is a self-checksum over the whole JSON document
(`hashlib.sha256` of the sorted-keys JSON with `sha256` itself absent -- see each
`goldens/gen_*.py`'s `main()`), so editing that text -- even though it changes no physical
value -- changes the file's checksum. Regenerated with `--reason` by re-running the actual
GMAT propagation (not hand-editing the JSON), and the full physical state vectors were
diffed byte-for-byte before vs. after: **identical in every field** (`initial_state`,
`state_pre_burn`, `state_post_burn`, `final_state`, `state_at_fault_demo_flt`,
`state_final_demo_flt`, `state_final_demo_flt_baseline_uncommanded`, `state_pre/post_burn_demo_mvr`,
`state_final_demo_mvr`, `state_pre_command_demo_flt`, `command_epoch_s`, `rmag_at_end_flt_m`,
`dv_*_km_s`, `tolerance_m`). No golden not listed here mentions `gmatviz` anywhere (verified
by grep over `goldens/*.json` before touching any of them); no other golden was regenerated.

| file | old sha256 | new sha256 | reason (recorded in the file) |
|------|------------|------------|--------|
| goldens/leo_1day_maneuver_vnb.json | 76f8d608c4a20461718190264f11bde555e423c2bd7bb92a00f5d4fb6bf261bb | 542226dd95de8de4c3ea2faff7094b6680f0c3331631389f4178eed4acc39dd4 | M26.1 (question 160): rename the old package/identifier prefix to altavista in golden metadata (note/reason text only); physics unchanged, regenerated to keep the file's self sha256 consistent |
| goldens/leo_1day_maneuver_ric.json | 8bf3229adaedc8206d9016f3886576208cb2613ccb3f43229f4f28f4a7b18158 | 22e457a93dc0c48b371922251cfccc4497f13c56ff054041d34e72b6bd69d6ac | (same reason) |
| goldens/leo_1day_maneuver_gmat_lvlh.json | 7eed8ee3d0759ca42524f172ff11efa04c6842993e7e051e80bfeb4971276a2b | 07c27d87f953fb9c78bea09a73bc9057763f9633a9bfd3ba99607520774e54b5 | (same reason) |
| goldens/demo_two_instance.json | 7f3f93f1917cbb53bf91937df24cbcc62ad7e27bee5e6f11e2ef877c95995173 | 32c67f6fc667c9c09ac5c442d1c01d37eff16aebf83d7e6aee9858bf757b08c2 | (same reason) |

Note: the reason text deliberately says "the old package/identifier prefix" rather than
spelling out the literal old name, so the final `grep -rn gmatviz` sweep stays clean without
leaving a self-referential exception to explain.

Also updated (plain text edit, no regeneration needed -- see justification): the three
viewer test fixtures `web/js/fixtures/{ric_axes,fixed_rotation,nadir_attitude}_fixture.json`
had `gmatviz` only in a human-readable `"groundTruth"` provenance string, no `sha256` or any
other hash/checksum field. Verified valid JSON after editing and that only the `groundTruth`
line changed (no numeric field touched).

### DRM/system `hash:` fields (rehashed with `cargo run -p av-kernel --example drm_hash`)

Every `drms/*.yaml` file that mentions `gmatviz` does so **only in YAML comments** (verified:
a script classified every matching line in `drms/*.yaml` by its trimmed leading character;
zero non-`#`-prefixed matches in `drms/leo_1day_maneuver_ric.drm.yaml`,
`drms/leo_1day_maneuver_vnb.drm.yaml`, `drms/leo_1day_maneuver_vvlh.drm.yaml`; no other
`drms/*.yaml` mentions `gmatviz` at all -- notably `drms/demo_two_instance.system.yaml` and
`drms/demo_two_instance_ctrl.system.yaml`, the two demo files the acceptance criteria name,
mention it **zero** times, comment or otherwise). `canonical_system_hash`/`canonical_drm_hash`
hash the *parsed* proto message with the hash field cleared, and YAML comments are stripped
by the parser before that, so editing only comments cannot change a `hash:` field. Confirmed
directly, stored value vs. freshly recomputed value, **after** the comment edits landed:

| file | stored `hash:` | recomputed (`drm_hash`) | changed? |
|------|------|------|------|
| drms/leo_1day_maneuver_ric.drm.yaml | 27695c5b7037cffa4db6d54f49416a86e27934364219c328e38eb5c8a6deb0bf | 27695c5b7037cffa4db6d54f49416a86e27934364219c328e38eb5c8a6deb0bf | no |
| drms/leo_1day_maneuver_vnb.drm.yaml | 744e8632256063a575edc37ab9646358d42a1ad10be3d2945b75a955aae97265 | 744e8632256063a575edc37ab9646358d42a1ad10be3d2945b75a955aae97265 | no |
| drms/leo_1day_maneuver_vvlh.drm.yaml | 1200da08c81044dda5710bd2945fa336557469ea0171d4920ee3cf20bd70bb7c | 1200da08c81044dda5710bd2945fa336557469ea0171d4920ee3cf20bd70bb7c | no |
| drms/demo_two_instance.system.yaml | 20ed412883a8e16bbcc2a625da852ffbe0b7954ad0d7d91ded42fd72f4ea7886 | 20ed412883a8e16bbcc2a625da852ffbe0b7954ad0d7d91ded42fd72f4ea7886 | no |
| drms/demo_two_instance_ctrl.system.yaml | 1a95524576b122961ab6a29b402582f146d730ef468107dffd2debe2a5b0835b | 1a95524576b122961ab6a29b402582f146d730ef468107dffd2debe2a5b0835b | no |
| drms/leo_1day_golden.system.yaml | bf49e03be4e63ced9b7442058544d427776e257af88bfeecb343b840220c0f25 | bf49e03be4e63ced9b7442058544d427776e257af88bfeecb343b840220c0f25 | no (spot check, no gmatviz mention at all) |

This directly confirms the acceptance criterion "the demo drives unchanged... confirm the
config hash is identical end to end" at the config-hash level, before even running the demo.

## Remaining `gmatviz` hits (final grep, with justification)

Full-repo grep (`.py .rs .js .mjs .md .yaml .yml .toml .json .html .css .proto .cpp .h .sh
Dockerfile`, excluding `.venv/ target/ build/ node_modules/ "GMAT R2026a"/ third_party/
__pycache__/`) after all edits above. Every remaining hit, by category:

1. **`proto/altavista/v1/trajectory.proto:6` and `proto/altavista/v1/packet.proto:44`** --
   the two comment-only mentions the task brief named explicitly ("leave them alone and list
   them in REPORT.md; the lead will decide"). `proto/**` is read-only to this task. Untouched.
2. **`third_party/rtems/REPORT.md:70` and `third_party/renode/REPORT.md:6`** -- the two files
   under `third_party/` the brief named explicitly (both just list `gmatviz/` among directories
   *not* touched by that other, concurrent task). `third_party/**` is out of scope and an M24
   build is using it as evidence right now. Untouched.
3. **`docs/adr/000-scope-and-lineage.md`, `001-cdm-v1.md`, `002-dynamics-contract.md`,
   `005-simulation-kernel.md`** -- ADR text, explicitly read-only per the task brief
   ("`docs/adr/**` are READ-ONLY"), and squarely inside the acceptance criterion's own
   allowance ("ADR amendments"). Untouched. (`000-scope-and-lineage.md:61` in particular
   records question 3's *original* answer, "gmatviz remains the name of the Python GMAT
   lineage" -- since superseded by question 160, but the ADR is the historical record of that
   original decision, not a live instruction, and is read-only regardless.)
4. **`docs/teamlog/2026-09-02-m2.1-kernel-and-av-dynamics.md`,
   `docs/teamlog/adr-002-amendment-draft-drag-srp.md`, `docs/teamlog/2026-09-02-team-1.md`**
   (the last one, 30+ hits) -- a day-by-day team log narrating what happened at the time,
   naming files and modules as they were named *then*. Explicitly inside the acceptance
   criterion's allowance ("teamlog"). Rewriting history in a log would misrepresent what
   actually happened at each milestone; left untouched.
5. **`docs/open-questions.md`** (question 3's original answer at line 14, and every question
   78/97/102/106/122/160 mention of `gmatviz` in-line) and **`docs/ui-rework-plan.md`**
   (M26.1's own section, and question 160's answer as quoted there) -- **not** on the
   acceptance criterion's literal allow-list, so called out here individually rather than
   silently lumped in with teamlog/ADR. Judgment call: both files are a **decision record**
   (per `docs/open-questions.md`'s own header, "Answers get recorded inline"), not living
   reference documentation -- question 160's own answer text ("hashed identifiers with the
   `gmatviz.` prefix become `altavista.`") *is* the sentence describing this very rename, and
   rewriting it after the fact to say "the `altavista.` prefix become `altavista.`" would
   make the record nonsensical and misrepresent what question 160 actually decided. Treated
   the same way as teamlog and ADR text: a historical record of a decision, not touched. If
   the lead disagrees and wants past-tense narration updated to name new paths going forward
   (e.g. question 78/97/102/106/122's file-path mentions), that is a one-line reversal of this
   judgment call, not a re-litigation of the rename itself.
6. **`REPORT.md`** (this file) -- necessarily discusses the old name throughout since it is
   the record of this rename; not a code or doc artifact the acceptance grep is checking.
7. **`altavista/pb/generate.py`** -- checked again after the wording change made to avoid a
   self-referential exception in its own docstring (see the pb-collision finding above):
   **zero** remaining hits, confirmed by `grep -c gmatviz` returning 0.

No other file, anywhere in the tree, contains `gmatviz`. (Full command used for this sweep is
recorded in the "Gate output" section below alongside its result.)

## Gate output

- **`cargo build --workspace`**: green. `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 1m 01s`, all 11 crates compiled (av-cdm, gmat-sys, av-kernel, av-dynamics-service, av-run, av-dynamics, av-lockstep-shim, av-grpc, av-lockstep, openssl-sys/openssl/hyper-openssl deps).
- **`cargo test --workspace --exclude av-kernel`**: green, exit 0, every suite `0 failed` (av-cdm, av-run, gmat-sys, av-dynamics-service `convert`/`convert_rotation`/`drag_srp_stm`/`epoch_writeback`/`gmat_port_cd_command`/`leo_golden`/`model_stm`/`stm_spike` integration tests, all doc-tests).
- **`cargo test -p av-kernel`**: green, exit 0, **665 passed, 0 failed** -- exactly the baseline count, confirmed by summing every suite's own `N passed; 0 failed` line from the full captured output (drm_attitude_control_cfs, drm_attitude_sensors(+_fixture), drm_container, drm_executor, drm_maneuver, drm_shared_run, dropped_messages, expr_goldens, expr_objectives, faults_determinism, faults_seeded, gates_execution_error, golden_acceptance, port_command_events, port_command_rematerialization, ports_router, registry, restart_invariance, segment_merge, state_space_declaration, doc-tests -- including `a_state_space_shaped_like_altavista_cdm_py_emits_it_round_trips_through_the_kernel`, the renamed version of that test).
- **`cargo clippy --workspace --all-targets -- -D warnings`**: green, exit 0, zero warnings across the whole workspace including every test/example target.
- **`cargo deny check`**: green -- `advisories ok, bans ok, licenses ok, sources ok`. Only pre-existing `windows-sys` duplicate-version advisory-level *warnings* printed (0.52.0 vs 0.61.2, both pulled transitively through `tonic`/`prost-build`'s dependency tree on this host's target triple), unrelated to this rename and not gating (`cargo deny check` still reports overall success).
- **`.venv/bin/pytest -q`**: **354 passed, 1 failed** (baseline 355 passed). The one failure is `services/cfs/tests/test_image_digest.py::test_image_builds_and_digest_matches_recorded_value`, investigated above and confirmed pre-existing/unrelated to this task (the local Docker image it compares against already disagreed with the recorded digest before this session made any edit, and the Dockerfile copies nothing this rename touched). 354 + 1 = 355, i.e. the total test count is unchanged from baseline; only that one pre-existing failure accounts for the delta from "355 passed" to "354 passed". Two Docker-build tests that looked flaky on an earlier, resource-contended run (`tests/test_lockstep_ref.py`'s docker lifecycle test, and this same cfs digest test racing against a concurrent `cargo test`/`cargo clippy`) both came back clean once re-run in isolation with the environment settled -- see the investigation write-up above.
- **Full-repo `gmatviz` grep**: see "Remaining `gmatviz` hits" above -- every hit justified.

## New commands (for project memory note)

- Serve the viewer: `.venv/bin/python -m altavista serve --host 127.0.0.1 --port 8765`
  (was `python -m gmatviz serve`). CLI entry point form also works:
  `.venv/bin/altavista serve --host 127.0.0.1 --port 8765` (was `.venv/bin/gmatviz`).
- `.claude/launch.json`'s server configuration is now named `"altavista"` (was `"gmatviz"`),
  same args under `-m altavista serve`.
- Env var for the viewer client URL override: `ALTAVISTA_URL` (was `GMATVIZ_URL`,
  `altavista/client.py`'s `DEFAULT_URL`).
- Publish a Python-authored scenario: unchanged mechanics, new import --
  `import altavista as gv` (was `import gmatviz as gv`), then `gv.Scenario(...).publish()`,
  or `.venv/bin/python -m altavista run path/to/mission.script`.
- **Demo drive, exactly as run for this report** (both system files are required, per the
  task brief):
  ```bash
  cargo build -p av-run
  target/debug/av-run \
    --drm drms/demo_two_instance.drm.yaml \
    --sos drms/demo_two_instance.sos.yaml \
    --system drms/demo_two_instance.system.yaml \
    --system drms/demo_two_instance_ctrl.system.yaml \
    --run-id <id> \
    --server http://127.0.0.1:8765   # viewer must already be serving (see above)
  ```
  Regenerate the committed protobuf Python bindings (only needed after touching `proto/**`,
  not part of ordinary use): `.venv/bin/python altavista/pb/generate.py`.

## Demo drive result

Ran exactly the "New commands" invocation above. `av-run` output:
`run_id="m26-demo-drive" config_hash=c10e51b464424c2a210833eecfced042ce686f87e0b590bcb947d7d1068fbbd3
trajectories=2 events=10`, then `POST http://127.0.0.1:8765/api/cdm/run -> HTTP 200:
{"ok":true,"name":"run:m26-demo-drive","clients":3}`.

**Config hash identical end to end, confirmed two independent ways:**
1. Directly: every YAML input to this run (`demo_two_instance.drm.yaml`,
   `demo_two_instance.sos.yaml`, `demo_two_instance.system.yaml`,
   `demo_two_instance_ctrl.system.yaml`) was grepped and **none ever mentioned `gmatviz`**,
   comment or otherwise -- confirmed again just now with a fresh grep. `crates/av-kernel/src/drm/hash.rs`
   (the hashing code itself) was never touched (`grep -c gmatviz` on it: 0). So `config_hash`
   is a pure function of unchanged inputs and unchanged code -- nothing *could* have changed it.
2. Empirically: the standalone `cargo run -p av-kernel --example drm_hash` check earlier in
   this report recomputed each of the two `system.yaml` files' own `hash:` field after the
   comment-only edits and got the exact stored value back (see the DRM/system hash table).

**Scene renders**, verified visually in the browser at `http://127.0.0.1:8765/` after the
POST: page title and sidebar header read **"Alta Vista"** (the two-word display string, per
question 160); scenario selector shows `run:m26-demo-drive`; the sidebar's own scenario-info
line reads `EarthMJ2000Eq · 2.0 h · 2 spacecraft · config c10e51b464424c2a210833eecfced042ce686f87e0b590bcb947d7...`,
i.e. the browser is displaying the **same config hash** `av-run` printed; Earth renders with
both spacecraft trajectories (`demo_flt`, `demo_mvr`, 121 points each) drawn as orbits; the
Events panel lists `run_start`/`fault1`/`burn1`/`Cd`/`run_end` for each instance including
`demo_ctrl` (timeline-only, correctly drawn with no 3D position -- question 133/140/141's
fix, unaffected by this rename); the Frames dropdown offers `EarthBodyFixed`, `EarthICRF`,
`EarthMJ2000Eq`, `demo_flt_body`, `demo_mvr_body`; the HUD line at the bottom of the 3D view
reads `run:m26-demo-drive · EarthMJ2000Eq · origin`. A screenshot was taken during this
session confirming all of the above rendered correctly.

## Physics unchanged -- confirmed

Every physics figure checked is byte-for-byte or assertion-identical to before the rename:

- The four regenerated physics goldens' full state vectors (position/velocity at every named
  epoch, `command_epoch_s`, `rmag_at_end_flt_m`, delta-v vectors) diffed **identical** before
  vs. after regeneration (see the hash change table above).
- Every DRM/system `hash:` field checked recomputed to the **same stored value** (comments
  don't reach the parsed message).
- `cargo test -p av-kernel --test demo_two_instance -- --nocapture` prints the task brief's own
  cited figure **exactly**: `[demo_two_instance] demo_flt final: |dr| = 0.0108 m (tol 0.05),
  |dv| = 1.611e-5 m/s (tol 0.00005)` (plus `demo_mvr post-burn`/`demo_mvr final`, both
  `|dr| = 0.0000 m`). Bit-for-bit the same residual the brief names, against the same
  `0.05 m` tolerance, never loosened.
- The live demo drive (`av-run` against the real DRM/SOS/system YAMLs, not goldens) produced
  a config hash identical to what the unchanged inputs and unchanged hash code guarantee, and
  rendered correctly in the browser (see "Demo drive result" above).

No physical value was observed to move. If one had, this section would say so and the task
would stop per the brief's explicit instruction.

## Anything not done and why

Everything in the task brief was completed and confirmed: gates all green with exact
baseline-matching counts (`cargo test -p av-kernel`: 665 passed 0 failed;
`.venv/bin/pytest -q`: 354 passed + 1 pre-existing unrelated failure = 355 total), physics
confirmed unchanged both structurally (unchanged hashes/inputs) and by the exact `|dr| =
0.0108 m` figure the brief names, demo driven end to end with an identical, verified config
hash and a rendered scene. Smaller items handled with a disclosed judgment call rather than
left silently equivalent:

- **The three viewer test fixtures' `groundTruth` text** (`ric_axes_fixture.json`,
  `fixed_rotation_fixture.json`, `nadir_attitude_fixture.json`) were fixed by direct text edit
  rather than by re-running their `gen_*.py` generators against real GMAT, since they carry no
  hash/checksum field and only a human-readable provenance string changed (verified: only that
  one line differs per file, valid JSON, no numeric field touched). Disclosed rather than
  silently treated as equivalent to a golden regeneration.
- **`services/cfs/tests/test_image_digest.py` failure** was investigated to a confident root
  cause (pre-existing, unrelated to this task -- see the dedicated write-up above) but not
  "fixed", because fixing it would mean either rebuilding `third_party/cfs`-derived state (out
  of scope, explicitly off limits while M24 uses it as evidence) or editing
  `services/cfs/IMAGE_DIGEST.md`'s recorded value (not this task's call to make). Left for the
  lead.
- **Two proto-comment mentions and two `third_party/` mentions** left untouched, exactly as
  instructed, and listed above.
- **`docs/open-questions.md` and `docs/ui-rework-plan.md`**: left untouched as a historical
  decision record (judgment call, explained above); not on the acceptance criterion's literal
  allow-list, so flagged explicitly rather than silently bundled in with teamlog/ADR.
