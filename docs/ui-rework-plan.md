# UI rework: Alta Vista naming and a collapsible, tileable windowing system

Requested by the user 2026-09-06; decisions in `docs/open-questions.md` section I
(questions 160–163). Runs in parallel with M24 (Renode), which is serial and spends most of
its wall time waiting on builds; this work touches `web/` plus the mechanical rename.

## Decisions

- **Name: everything, including the package.** The product is displayed as "Alta Vista"
  (two words). The Python package, CLI and server become `altavista` (`python -m altavista
  serve`); state-space ids and other hashed identifiers that carried the `gmatviz.` prefix
  become `altavista.`, with every affected golden regenerated with `--reason` and the DRM
  hashes rehashed. This supersedes question 3's "gmatviz stays the name of the Python GMAT
  service lineage".
- **Windowing: hand-rolled tiling.** A binary split tree of panes with drag handles,
  collapse-to-rail, and layouts as data (default layouts per profile, persisted per viewer
  in local storage, exportable and importable as JSON). No dependency, no build step.
- **Panels in v1:** multiple 3D viewports (each with its own view frame, focus and floating
  origin, sharing one scene and one clock), run products and scores, a 2D companion map
  (ground tracks and footprints on an equirectangular map from the same imagery profile),
  and a console/log panel (connection, server messages, run provenance and hashes).
- **Timing: now.**

## Milestones (M26, web-only after the rename)

**M26.1 Rename.** Repo-wide `gmatviz` → `altavista` (package, CLI, server, tests, docs,
launch config, viewer title and HUD "Alta Vista"), hashed identifiers renamed with goldens
regenerated and every hash change listed in the report; the old package name is not kept
as an alias. Lands before any other M26 task and while no other crate edit is in flight.

**M26.2 Windowing core.** `web/js/layout/`: split tree, panes, drag handles with keyboard
equivalents, collapse to a rail, layout persistence and JSON import/export, a default layout
per profile. Headless test of the tree operations (split, collapse, restore, serialize round
trip) and a browser check. Existing sidebar content moves into panes unchanged.

**M26.3 Multiple 3D viewports.** One scene, many cameras: each viewport owns a view frame,
focus, floating origin and camera; labels and picking per viewport; shared clock. The
precision harness runs per viewport and the RPO figure stays bit-identical. Default layout
for the RPO profile: ICRF beside RIC beside globe.

**M26.4 Panels.** Run products and scores (objectives with pass state, measures, port
command and fault events linked to the timeline); 2D companion map (ground tracks,
footprints, current positions, same imagery profile, offline fixture); console/log.
Headless tests for each panel's data binding against the ingested demo run.

**Exit.** The two-instance demo and the attitude-control demo both load into the default
layout, every panel is collapsible and re-tileable, a saved layout round-trips, and the
viewer's precision, frame and event checks from earlier milestones all still pass.
