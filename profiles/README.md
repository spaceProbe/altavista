# `profiles/`

The first real profile files (M7.2), per `docs/architecture.md` section 5, "Configurability:
four profiles": "Profiles are files first (a console that edits the same files comes later).
They select components, plugins, trees, viewer layers, labels and authority policy; they
never change numerical behaviour." That second sentence is ADR-000 rule 1, restated -- nothing
in this directory may set a rate, a tolerance, or a force-model choice; those live in DRMs and
`SystemDefinition`s (`proto/altavista/v1/system.proto`) and are hashed there (`drms/README.md`
"Hashing"), never here.

Four files, one per column of architecture.md section 5's table: `design.yaml`,
`feasibility.yaml`, `analysis.yaml`, `execution.yaml`.

## Schema (informal -- no proto message backs this yet)

No `Profile` proto message exists anywhere in `proto/**` (checked: no `profile` field or
message in any `.proto` file except a passing mention inside `Provenance.config_hash`'s doc
comment, "the configuration (DRM, system definition, profile) the artifact came from" -- a
hash *of* a profile someday, not a schema for one). Consistent with "files first," these are
plain, hand-authored YAML with no generated bindings, hashing, or validation tooling yet --
that is future work for whoever builds the console architecture.md's own sentence anticipates.
Each file:

```yaml
id: <profile id>
name: <human name>
architecture_ref: <which architecture.md section 5 column this mirrors>
rule: "Profiles select components; they never change numerical behaviour (ADR-000 rule 1)."

imagery:
  url_template: <XYZ/WMTS tile URL template, {z}/{x}/{y} placeholders>
  attribution: <attribution string, shown in the viewer>
  max_level: <deepest zoom level this source's tile pyramid actually covers>

dynamics_backend:
  - name: <component name>
    kind: <rust_service | rust_crate | python_service | web_app>
    path: <repo-relative path>
    role: <what it does, free text>

planes:
  ingestion: { per_architecture: <table cell text>, components: [...], status?, note?, not_yet_built? }
  hot_track: { ... }
  heavy_track: { ... }
  viewer: { ... }
  ai: { ... }
  command: { ... }
```

- **`imagery`** (M19.5, question 132) is the globe's tile source: an XYZ/WMTS URL
  template (`{z}`/`{x}`/`{y}` placeholders, substituted verbatim -- `web/js/globe.js`'s
  `urlForTile`), an attribution string the viewer actually displays (`web/js/app.js`'s
  globe panel), and the source's own declared max zoom level. Every profile defaults to
  the same offline fixture (`web/fixtures/gen_globe_tiles.py`) -- question 132 left
  which public/restricted basemaps to ship per profile as a later user decision. Not a
  plane and not a `dynamics_backend`-style component list: a profile *selects* the
  imagery source (question 11's rule), it does not change how the globe behaves once it
  has one -- LOD thresholds, tile budget and resident-cache size stay hardcoded in
  `web/js/globe.js`, never here.
- **`dynamics_backend`** is not a plane -- it is the one component every profile lists
  (`crates/av-dynamics-service`, question 85 / ADR-003's 2026-09-02 amendment): the deployed
  Rust `DynamicsService` host every plane's numerical work can call, in every profile,
  Execution included by design (that amendment exists precisely so the Python `gmat-service`
  never has to be).
- **`planes`** has one entry per architecture.md section 5 row: `ingestion`, `hot_track`,
  `heavy_track`, `viewer`, `ai`, `command`. `per_architecture` is that row's own text for this
  profile's column, copied verbatim, so a reader can check this file against the table
  directly. `components` lists what this repository actually has today for that cell --
  `[]` when nothing does.
- **`status: not_yet_built`** (on a plane, or nested `not_yet_built: [...]` bullets) means
  exactly that: architecture.md's table calls for something no crate, service, or module in
  this repository implements yet. `status: partial` means some but not all of the cell's own
  text is covered by an existing component -- the `not_yet_built` list (or the component's own
  `role`) says which part is missing. This directory would rather say "not built" plainly than
  invent a service name that does not exist (this task's own binding rule).
- **`kind`** is a small, closed vocabulary (`rust_service`, `rust_crate`, `python_service`,
  `web_app` today) -- `tests/test_profiles.py` reads it structurally (not by pattern-matching
  file text) to check that the execution profile never lists a Python-hosted component; see
  that test's own docstring for exactly how.

## What architecture.md section 5 calls for that does not exist yet in this repository

Named once here rather than only scattered through four files' own `not_yet_built` lists:

- **Any sweep/Monte Carlo/job-runner component** (Feasibility's heavy track) -- `av-kernel::
  drm::execute` runs exactly one `DesignReferenceMission` per call; nothing in this repository
  orchestrates repeated runs over swept parameters yet (architecture.md phase P1 exit
  criteria: "parameter sweeps on the job runner").
- **MoE/Objective evaluation** -- `DesignReferenceMission.objectives`/
  `measures_of_effectiveness` parse and hash but are not evaluated; no metric-expression
  grammar exists (ADR-005 is Proposed, not accepted).
- **Any replay component** (Analysis's ingestion, hot track, viewer) -- no replay-from-log
  ingestion, no replay engine, no replay-with-truth-overlay viewer mode.
- **Any envelope/outlier/trade-dashboard or scorecard/tiling viewer or heavy-track component**
  (Feasibility's and Analysis's viewer/heavy-track cells).
- **SIL/HIL bindings** (Execution's ingestion and part of its hot track) -- `BINDING_KIND_
  CONTAINER`/`_RENODE`/`_BOARD` are explicitly refused by `av-kernel`'s DRM executor today
  (`DrmError::UnsupportedBinding`); ADR-005's runtime for them is Proposed, not accepted, and
  this task's own instructions say not to implement its scheduler refactor.
- **Any AI-plane component** (planners, surrogates, sidecars, proposals) -- architecture.md
  section 8 places the AI plane at phase P4.
- **Any Command-plane component** -- `proto/altavista/v1/command.proto` declares the message
  schema only; no state machine, OPA integration, or dispatch path is implemented anywhere in
  this repository. Also phase P4.

None of this is invented or stubbed in `profiles/*.yaml` to make a cell look filled -- each
gap is named, with a pointer to why (a proposed-but-not-accepted ADR, a later phase, or simply
"not built yet").
