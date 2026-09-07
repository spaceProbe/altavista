# M26.2 Windowing core -- report

status: done

Written incrementally, findings first, per this task's brief. Do not trust the
"final chat message" -- this file is the record.

This file was left at `status: in progress` with every box unticked by the worker
that wrote everything below "## Findings log" through "## Break-and-restore evidence"
-- that worker built and tested the whole thing but was interrupted before updating
this header. The M26.2-finish pass (this section and everything below "## M26.2-finish
pass" at the bottom) verified that prior work is real (re-ran the break-and-restore
steps itself, spot-checking two of the ten independently rather than trusting the
log), did the one genuinely missing required item (the interactive browser check),
found and fixed one real bug the browser check surfaced (keyboard-resize focus loss,
below), and brought this header up to date.

## Done

- [x] split_tree.js (pure tree model + ops + serialize/validate)
- [x] persistence.js (localStorage load/save, corrupt-JSON handling)
- [x] default_layouts.js (default tree + per-profile-imagery lookup)
- [x] layout_manager.js (DOM render, drag handles, keyboard equivalents, toolbar)
- [x] index.html / style.css wiring (move #sidebar/#viewport into panes)
- [x] web/js/layout/layout_tree_check.mjs headless harness
- [x] tests/test_viewer_layout.py pytest wrapper
- [x] break-and-restore evidence per test (10 cycles below; 2 independently
      reproduced during M26.2-finish as a check on the log itself, not just trusting
      the prior worker's attribution -- see "M26.2-finish pass" below)
- [x] browser check (real drag, real render, real keyboard) via Claude_Browser tool
      against the actual `.venv/bin/python -m altavista serve` server + a live
      published scenario -- see "M26.2-finish pass" below
- [x] full pytest run before/after, globe+jitter regression check -- 377 passed
      (355 pre-existing + 22 this task), no regressions
- [x] RPO figure bit-identical confirmation -- 3.385366653674282e-06 m, unchanged

## Not done / explicitly deferred

- Only two real content panels exist (`sidebar`, `viewport`, moved unchanged into
  panes -- this task's brief). Splitting a pane creates a real new leaf, but with no
  third panel type yet it gets a `placeholder-N` panelId and placeholder body text.
  Building real M26.3/M26.4 panels is out of scope for the windowing core itself.
- Per-profile default layouts cannot actually differ yet -- see "Profile imagery is
  NOT currently profile-distinguishing" below; the lookup mechanism is real and
  tested, there is simply only one signature in the repo today to key it on.
- `layout_manager.js` (the DOM-rendering module) is not exercised by
  `tests/test_viewer_layout.py` -- it needs a real `document` and this repo adds no
  jsdom/browser-test dependency (question 161: no new dependency). It is verified
  instead by the interactive browser check below, which is necessarily a one-time,
  human/agent-witnessed check rather than something CI re-runs on every commit. If a
  future milestone wants that automated, it needs an explicit decision to add a DOM
  test dependency -- not done here without that decision.

## Findings log

### Profile imagery is NOT currently profile-distinguishing (read before building default-per-profile)

Read `altavista/profile.py` and `altavista/server.py` (read-only, per scope -- did not
edit): `Hub.__init__(imagery=...)` stamps the *active* profile's `imagery` block
(`{urlTemplate, attribution, maxLevel}`) onto `scenario.imagery` when publishing --
that is the only profile-derived signal that reaches the client at all. There is no
`profile id`/`name` field sent to the viewer today, and adding one is a server-API
change ("anything touching the server API surface comes to the manager first --
do not write it"), so it is out of scope here.

Checked all four `profiles/*.yaml` (`feasibility`, `design`, `execution`, `analysis`):
their `imagery:` sections are **byte-identical** today --
`url_template: './fixtures/tiles/{z}/{x}/{y}.png'`, same `attribution` string, same
`max_level: 2`. So even a correct client-side "pick a default layout keyed by
`scenario.imagery`" mechanism cannot actually *distinguish* one profile from another
right now -- there is only one distinct imagery signature in the whole repo.

Decision: build `defaultLayoutForImagery(imagery)` as a real, generic, extensible
lookup keyed by a signature of `{urlTemplate, maxLevel}` (the fields that identify a
*source*, not the human-readable `attribution` text), with a documented fallback to
one generic default tree when no signature matches. Today that means every profile
resolves to the same default layout -- which is the honest, correct behaviour given
what the server currently sends, not a shortcut. The lookup mechanism itself is real
and tested (registering a second signature and getting a different tree back), so it
is ready the day a profile's imagery actually diverges or a real `profile` field is
added by whoever owns the server surface.

## The tree model and its operations (web/js/layout/split_tree.js)

A pane is a `leaf` (`{type:'leaf', id, panelId, collapsed}`) or a `split`
(`{type:'split', id, direction:'row'|'column', ratio, children:[a,b]}`). All
operations are pure (return a new tree, never mutate input):
`splitLeaf`, `closePane` (sibling takes the space, discarding the parent split node;
closing the tree's last leaf throws), `resizeSplit` (clamps to `[0.05, 0.95]`),
`collapseToRail`/`restoreFromRail` (toggle `collapsed`, never touch `ratio`).
`validateTree`/`deserializeLayout` reject anything malformed (`LayoutValidationError`,
never a silent default) -- bad JSON text, wrong child count, duplicate ids,
out-of-range ratio, unknown type/direction, missing panelId.

## Serialization and persistence (web/js/layout/persistence.js)

`serializeLayout`/`deserializeLayout` (plain `JSON.stringify`/`JSON.parse` + full
validation) are "layouts as data" (question 161). `persistence.js` wraps these with
injectable `storage` (real `localStorage` in the browser, an in-memory stand-in in
tests -- this is why the whole layout core runs headlessly under plain `node`, no
DOM, no jsdom dependency added). `loadLayout()` always returns something renderable,
but on a corrupt stored value it returns a non-null `error` and -- critically --
**never calls `storage.setItem` itself**: the corrupt string is left exactly as it
was. `web/js/layout/layout_manager.js` is the only place that turns that `error` into
a visible `#layout-error` banner; it never overwrites `localStorage` except on an
explicit user action ("Discard and use default" / a new drag / import). Export/import
as JSON (`exportLayoutJson`/`importLayoutJson`) go through the same validating
`deserializeLayout`, so an imported file gets the identical "no silent fallback"
guarantee.

Storage key: `altavista.layout.v1` (`localStorage`, per-viewer/per-browser-profile,
matching "persisted per viewer" in question 161 -- there is no server-side layout
storage, and none was added).

## Default layout per profile (web/js/layout/default_layouts.js)

Read `altavista/profile.py` and `altavista/server.py` before building this (see
"Findings log" above): the only profile-derived signal that reaches the client at all
is `scenario.imagery` (`{urlTemplate, attribution, maxLevel}`), and all four shipped
`profiles/*.yaml` files declare byte-identical imagery today. `defaultLayoutForImagery
(imagery)` is a real lookup keyed by a signature of `{urlTemplate, maxLevel}`
(`attribution` excluded -- it is descriptive text, not a source identity), with one
registered entry (the offline fixture -> the base sidebar+viewport split) and a
documented fallback to that same base layout for anything unregistered. This is not a
stub: `registerDefaultLayoutForImagery` is exercised by a test that registers a
second signature and checks the result actually changes
(`defaultLayout.registeringANewSignatureActuallyChangesTheResult`). No new scenario
field was added and no server file was touched.

## Break-and-restore evidence (node web/js/layout/layout_tree_check.mjs after each edit)

Every row: introduced exactly the described one-line/small change, ran the harness,
recorded which check(s) flipped to `pass:false` (or, for the first row, that the
process crashed instead of the specific check even running), then reverted. All
breaks were isolated to `split_tree.js`/`persistence.js`/`default_layouts.js`
one at a time; the harness output between breaks was re-confirmed
`allPass:true` (40/40) and the two-independent-run raw stdout byte-identical.

1. **split -- discard original leaf** (`splitLeaf`'s `children` built as
   `[newLeaf, newLeaf]` instead of `[node, newLeaf]`): the harness process **crashed**
   (uncaught `LayoutValidationError: closePane: both children resolved to removed --
   corrupt tree (duplicate ids?)`) before printing JSON at all -- `pytest
   tests/test_viewer_layout.py` turned from 22 passed into **1 passed, 21 errors**.
   Restored; back to 22 passed.
2. **split -- rename original leaf's id** (`{...node, id: genId('leaf')}` instead of
   keeping `node`): exactly `split.originalLeafSurvivesWithSamePanelId` and
   `close.survivingSiblingIsUnchanged` flipped to fail. Nothing else. Restored.
3. **close -- remove the last-pane guard**: exactly `close.rejectsLastPane` and
   `close.rejectsLastPaneWithLayoutValidationError` flipped. Restored.
4. **resize -- drop `clampRatio()`** (`ratio: ratio` instead of `clampRatio(ratio)`):
   exactly `resize.clampsAboveMax` and `resize.clampsBelowMin` flipped. Restored.
5. **collapse -- reset ancestor `ratio` while recursing** (simulating a collapse
   implementation that touches the split it walks through): exactly
   `collapse.preservesAncestorRatio`, `collapse.restorePreservesAncestorRatioExactly`,
   `collapse.restoreReproducesPreCollapseTreeExactly` flipped. Restored.
6. **validateTree -- remove the ratio-range check entirely**: exactly
   `invalidRejected.ratioOutOfRange` and `invalidRejected.ratioZero` flipped (a
   corrupt saved layout with `ratio: 1.4` or `ratio: 0` would otherwise be silently
   accepted). Restored.
7. **createLeaf -- omit `collapsed` key when false** ("omit falsy defaults"): **did
   not** fail any `roundtrip.*` check (both sides of that comparison are built by the
   same buggy `createLeaf`, so they agree with each other) -- it only fails
   `collapse.restoreReproducesPreCollapseTreeExactly` (which compares a
   freshly-built leaf against one that went through `collapseToRail`+
   `restoreFromRail`, which always writes an explicit `collapsed` key via a
   spread, so the two disagree under the bug). This was a real gap in my own
   first-draft docstring, caught by actually running the break rather than assuming
   it -- corrected in `layout_tree_check.mjs`'s own comment so it doesn't
   misattribute coverage. Restored.
8. **persistence -- "self-heal" a corrupt stored layout** (call `saveLayout` with the
   default the moment `deserializeLayout` throws): exactly
   `persistence.corruptStoredStringIsNeverOverwritten` flipped. Restored.
9. **default_layouts -- ignore the registry** (`defaultLayoutForImagery` always
   returns `buildBaseSidebarViewportLayout()`): exactly
   `defaultLayout.registeringANewSignatureActuallyChangesTheResult` flipped.
   Restored.
10. **genId -- seed with `Math.random()`**: the existing harness had *no* call site
    that exercised the auto-id fallback (every call passes an explicit id), so this
    change alone produced **zero** diff between two runs -- a real gap, fixed by
    adding a genuine unlabelled `splitLeaf(base, 'pane-sidebar', 'row', 'notes')` call
    (`split.autoGeneratedIdsUseThePlainPrefixedCounterScheme`, the shape a real
    "click to split" UI action takes, since the user never types an id). With that
    call in place, the same `Math.random()` injection produced a non-empty `diff`
    between two independent `node` runs (the determinism test's exact failure mode)
    **and** flipped the new check to fail. Restored (both the bug and confirmed the
    new check is now a permanent, real regression guard rather than a name with
    nothing behind it).

After all ten: `node layout_tree_check.mjs` run twice back-to-back diffed byte-identical,
`allPass:true`, 40/40 checks; `.venv/bin/pytest -q tests/test_viewer_layout.py` ->
**22 passed**.

## M26.2-finish pass (this task)

### Spot-checking the break-and-restore log above (not just trusting it)

Per this task's standing rule ("a worker's attribution of a failure is not evidence;
the artifact is"), independently reproduced two of the ten cycles above myself before
relying on the rest of the log:

1. Removed `closePane`'s last-pane guard (item 3 above) -- ran
   `.venv/bin/pytest -q tests/test_viewer_layout.py`: **2 failed, 20 passed**, and the
   two failures were exactly `close.rejectsLastPane` and
   `close.rejectsLastPaneWithLayoutValidationError`. Restored (`git`-equivalent: byte
   diff against a pre-edit backup copy came back empty); re-ran -> 22 passed.
2. Made `persistence.js`'s `loadLayout` "self-heal" a corrupt stored layout by calling
   `storage.setItem` with the default the moment `deserializeLayout` throws (item 8
   above) -- ran the same pytest command: **1 failed, 21 passed**, and the one failure
   was exactly `persistence.corruptStoredStringIsNeverOverwritten`. Restored (diff
   against backup empty); re-ran -> 22 passed.

Both reproductions matched the prior worker's log exactly (same check names, same
failure counts). Treating this as sufficient sampling rather than re-running all ten
-- the two chosen are the two the task brief calls out by name as the required tests
("an invalid layout JSON is rejected with a visible error" / "not silently replaced by
a default"), so they are the highest-value ones to verify independently.

### Browser check (the item actually missing)

Server: the existing `.claude/launch.json` "altavista" config
(`.venv/bin/python -m altavista serve --host 127.0.0.1 --port 8765`) -- a copy was
already running (another session's preview server, `gmatviz`, same repo, same
command) with a real scenario published (`run:m26-demo-drive`, 2 spacecraft), so the
check ran against a live, non-trivial scenario rather than the empty-viewer state.
Driven via the Browser pane tools, matching how `web/VIEWER.md`'s own "browser
verification" sections work.

**Render**: loaded `http://localhost:8765/`. Screenshot confirmed the tiled layout
actually renders: a "Sidebar" pane and a "3D View" pane side by side, a visible drag
handle between them, the toolbar (Export/Import/Reset layout) above, and the real
live scenario (Earth, two spacecraft, event list, connected badge) inside the
sidebar/viewport panes -- i.e. `web/js/layout_bootstrap.js`'s "re-parent, don't
clone" contract is real: `app.js`'s existing UI is running unmodified inside a pane.

**Drag resize**: found the resize handle (`role="separator"`, `.av-handle`),
read its `aria-valuenow` (44), performed a real `left_click_drag` from the handle to
100px further, and confirmed via a follow-up screenshot and `aria-valuenow` (62) both
that the value changed and that the sidebar pane visibly widened on screen. (Note:
this pane's coordinate frame runs at 2x the CSS-pixel/DOM coordinate frame reported
by `getBoundingClientRect()` -- confirmed once via an instrumented click before
trusting any further coordinate, not assumed.)

**Keyboard equivalents (question 161's "both drag handles and keyboard equivalents"
requirement) -- and a real bug found and fixed here**: focused the handle
(`.focus()`, confirmed `document.activeElement === handle`) and sent real OS-level
key presses (not JS-dispatched events) via the Browser pane's `computer` tool:

- `ArrowLeft` on a row-direction handle: `aria-valuenow` 62 -> 60 (the documented
  `RATIO_STEP = 0.02`).
- Sent **three** `ArrowLeft` presses in a row (the realistic way a keyboard user
  actually uses this: several taps, not one) and checked focus after **every** press,
  not just the value. **First run (before the fix below) lost focus after exactly one
  press** -- `aria-valuenow` updated correctly (62 -> 60) but `document.activeElement`
  became `<body>`, so the second `ArrowLeft` was silently a no-op. Root cause:
  `LayoutManager.render()` does `this.root.innerHTML = ''` and rebuilds the whole
  subtree on every change (including a keyboard resize step), which destroys the
  focused handle's DOM node and replaces it with a new one; nothing re-focused the
  replacement. A mouse-only user never notices this (they re-click the handle every
  time anyway), but it makes the keyboard equivalent effectively single-step only --
  a real, user-facing defect in "keyboard equivalents ... exist," not just a
  theoretical gap.
- **Fix** (`layout_manager.js`, `render()` / new `_captureFocus()` / `_restoreFocus()`
  helpers): before tearing down the DOM, note if the focused element is `.av-handle`
  and which `splitId` it belongs to; after rebuilding, look up the handle with that
  same `data-split-id` in the new tree and refocus it (a no-op, not a crash, if that
  split no longer exists -- e.g. it was closed elsewhere).
- **Re-verified after the fix**, reloading the page fresh: focused the handle, then
  `ArrowLeft` x1 (62 -> 60, still focused), `ArrowLeft` x2 more (60 -> 56, still
  focused), `Home` (-> 5, the documented `MIN_RATIO`, still focused), `End` (-> 95,
  the documented `MAX_RATIO`, still focused), `Enter` on the root handle specifically
  (-> 50, the documented reset-to-50/50, still focused). All five keyboard equivalents
  work and now survive repeated use without losing focus.
- One instrumentation note for anyone reproducing this: the `computer` tool's key
  name `"Return"` produced a `KeyboardEvent` with `e.key === ""` in this environment
  (not `"Enter"`), so it silently didn't match the handler's `ev.key === 'Enter'`
  check -- had nothing to do with the app. `"Enter"` as the key name produced the
  correct `e.key === 'Enter'` and worked. Recorded here so it isn't mistaken for a
  second bug by a future check.

**Split / close / collapse / restore, live**: clicking a pane's "Split below" button
(discovered as a side effect of an early mis-clicked coordinate during handle-position
calibration, before the 2x scale factor above was known) created a real new
`placeholder-N` pane, nested under the sidebar in a new column split, which persisted
through a page reload (`localStorage`) -- incidental extra confirmation that
`splitLeaf` and persistence both work outside the headless harness too. Then,
deliberately: clicked "Collapse to rail" on that pane -- screenshot confirmed it
shrank to a thin labeled rail; clicked the rail to restore -- screenshot confirmed it
came back at its original size (the split's `ratio` was never touched, matching
`collapse.preservesAncestorRatio`'s guarantee); clicked "Close pane" on it -- its
sibling (the sidebar) took the freed space, back to the clean two-pane baseline,
matching `closePane`'s "sibling takes the space" contract.

**Invalid layout JSON rejected with a visible error, live (not just in the headless
harness)**: set `localStorage['altavista.layout.v1']` to `'{not valid json!!'`
directly (the real-world corruption scenario -- a hand-edited or half-written
`localStorage` value) and reloaded. Screenshot confirmed a visible red banner:
*"Saved layout is corrupt and was NOT modified (altavista.layout.v1): invalid layout
JSON: Expected property name or '}' in JSON at position 1 (line 1 column 2). Showing a
default layout instead -- use 'Reset layout' to overwrite the saved one."* -- and the
app still rendered a usable default two-pane layout underneath it (never blank, never
crashed). Read `localStorage` back afterward: still exactly `'{not valid json!!'`,
byte-for-byte -- confirms `persistence.js`'s "never call `storage.setItem` on a
corrupt value" guarantee live, not just under `node`. Clicked "Reset layout" to leave
the shared dev server in a clean state afterward (this explicitly overwrites via the
one sanctioned user action, exactly as the error banner's own text says to).

### Regression check

`.venv/bin/pytest -q tests/test_viewer_layout.py tests/test_viewer_globe.py
tests/test_viewer_jitter.py` -> **67 passed** (22 + 45), both before and after the
`layout_manager.js` focus fix (that file isn't exercised by pytest at all -- see "Not
done / explicitly deferred" above -- so this is confirming the fix didn't touch
anything the headless suite covers, not that the suite covers the fix itself).

Full suite: `.venv/bin/pytest -q` -> **377 passed** (355 pre-existing + 22 this task),
matching the stated baseline exactly, no regressions.

RPO figure: read directly off `node web/js/scene_jitter_harness.mjs`'s own JSON output
(`RPO.errWithM`) rather than trusting a truncated printed value --
`0.000003385366653674282`, i.e. **3.385366653674282e-06 m**, bit-identical to the
figure this task's brief states as the pre-existing baseline.

### Files touched by this pass

- `web/js/layout/layout_manager.js`: the focus-restore-after-render fix described
  above (`render()`, `_captureFocus()`, `_restoreFocus()`). Everything else in
  `web/js/layout/` is unchanged from the interrupted worker's version (confirmed by
  diffing `split_tree.js` and `persistence.js` against pre-spot-check backups after
  reverting the temporary breaks -- byte-identical).
- `web/js/layout/REPORT.md`: this file (header brought up to date, this section
  added).
- No changes to `tests/test_viewer_layout.py`, `web/index.html`, or anything outside
  `web/js/layout/` and this report.
