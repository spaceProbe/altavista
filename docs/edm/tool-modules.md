# EDM tool modules: the modular contract, and `cad-freecad` as module #1

**Status:** planning draft, 2026-09-28.

**Inputs:**
- `research/freecad-mcp.md`
- `compliance-crosswalk.md` (C06 analysis record, C07 tool accreditation, C14 labels, C16 AI
  provenance)
- `../edm-plan.md` §7

Nothing here is implemented yet. §7 lists the research spikes that come before any code.

## 1. Why a strict module contract

Every engineering tool is a separate module: CAD, meshing, structural, thermal, CFD, inspection
and document rendering. The goals:

- **Swap and add tools without touching the core.** For example, add build123d next to FreeCAD,
  or Ansys next to CalculiX.
- **Accredit each tool independently** (C07). A FreeCAD upgrade must not force the solver to be
  re-accredited, and the reverse holds too.
- **Keep AI providers out of the tools.** The module neither knows nor cares which agent, model
  or human called it. That keeps the tool plane intact whatever the model allow-list says (C16).
- **Make every call evidence.** One call is one hermetic job, and one job produces one
  attestation.

Survey finding: every existing FreeCAD MCP server is an *interactive copilot*. It keeps a stateful
FreeCAD session alive and gives callers an `exec` escape hatch. The contract below is deliberately
the opposite.

## 2. What a module is

```
tools/<module>/
├── tool.yaml              # manifest (below): identity, tools, schemas, limits, labels, licences
├── image/                 # Dockerfile / conda-lock or pixi lockfile; builds the pinned runtime
├── IMAGE_DIGEST.md        # the one home of the image digest (AltaVista convention)
├── entry/                 # job entrypoint run inside the container (e.g. freecadcmd entry.py)
├── mcp/                   # thin MCP server: validates args → job.json → submits to runner
├── schemas/               # JSON Schema per tool (inputs, params, outputs, extracted quantities)
├── goldens/               # accreditation suite: cases, expected values, justified tolerances
├── accreditation/         # intended-use statements and signed Accreditation records (C07)
├── sbom/                  # CycloneDX SBOM of the image + licence inventory
└── tests/                 # unit + headless integration (spkane's CI patterns)
```

**`tool.yaml`** is the declarative manifest. The gateway, runner and registry need nothing else
to host a module.

```yaml
module: cad-freecad
version: 0.1.0
upstream: { freecad: "1.1.4", occt: "7.8.x", python: "3.11" }   # recorded, verified at start
image: { ref: "registry.enclave/edm/cad-freecad", digest: "sha256:…" }
runtime:
  network: none
  rootfs: read-only
  user: nonroot
  limits: { cpu: 4, memory: 8Gi, wall_seconds: 900 }
  env: { QT_QPA_PLATFORM: offscreen, FREECAD_USER_HOME: /work/home, OMP_NUM_THREADS: "1" }
capability_tier: compute        # read | compute | propose  (§3.4)
tools:
  - name: build_model
    schema: schemas/build_model.json
    inputs: [model_source, params]         # all by sha256
    outputs: [fcstd, step, brep, stl, gltf, model_report]
    extracted: [mass, volume, area, com, inertia, bbox]
    intended_use_ids: [IU-CAD-GEOM-01]
  # …
labels: { max_input_label: "CUI//SP-EXPT", propagate: true }     # C14
licences: [LGPL-2.1-or-later (FreeCAD), LGPL-2.1-with-exception (OCCT), MIT (reused code)]
```

## 3. The job contract (identical for every module)

### 3.1 Life cycle

```
caller (agent or human, via edm-gateway MCP)
  │ tool call (args)                                   ← OIDC identity, label clearance
  ▼
module MCP server ── validates args against schema ── resolves inputs to digests
  │ job.json {job_id, module, tool, tool_version, image_digest, params, inputs[{name,sha256,label}],
  │           requested_by{principal, author_kind, agent{provider,model,version}?}, intended_use}
  ▼
runner ── pulls inputs by digest (verify size+sha256 on read) ── fresh container, --network=none
  │        tmpfs /work, inputs read-only, limits enforced, clock recorded (not used for outputs)
  ▼
entrypoint ── performs exactly one tool ── writes outputs + result.json (extracted quantities w/ units,
  │           diagnostics, validity gate verdict)
  ▼
runner ── normalizes outputs (§3.3) ── hashes raw + normalized ── stores (content-addressed)
  │        ── in-toto Statement (predicate edm.dev/<kind>/v1) ── signs (private Sigstore / HSM)
  │        ── appends to hash-chained evidence log ── labels outputs = max(input labels)
  ▼
Job record (edm.v1 Job) returned to caller
```

### 3.2 Invariants

These are enforced by the runner, not by module goodwill.

1. **Hermetic.**
   - The runner starts a fresh container for every job.
   - The container sees only the inputs named by digest.
   - It has no network, no persistent state and no user config.
2. **No inline code.**
   - No tool accepts source code as an argument.
   - Code (models as code, macros) is always an **input by digest**, and it must be on the
     module's allow-list. The allow-list is populated only by merged, reviewed PRs.
   - An agent that wants new code has to propose a PR.
3. **One tool per job.**
   - Multi-step work is a DAG of jobs, each attested. It is never a session.
4. **The validity gate is mandatory.** A job fails, rather than returning degraded output, when
   any of these happen:
   - a recompute failure;
   - an invalid shape;
   - a solver that did not converge;
   - a limits-of-operation violation.

   Failures are typed and early, following AltaVista's rule.
5. **Units always.**
   - Every extracted quantity carries a unit (the `altavista.v1` `Unit` set, SI).
   - Tools that work in mm internally (FreeCAD) convert at the boundary. The conversion is
     declared in the manifest.
6. **Labels propagate.**
   - Each output's label is at least as restrictive as the most restrictive input label.
   - The runner refuses a job whose inputs exceed the module's `max_input_label` or the caller's
     clearance.

### 3.3 Determinism and normalization

Bit-identical outputs are expected only for the same image on the same CPU architecture. The
contract therefore separates two things:

- **Raw digest:** the bytes as produced. This is the storage identity.
- **Normalized digest:** computed after stripping known non-determinism. This is the
  reproducibility check. Things stripped:
  - ZIP entry mtimes;
  - FCStd `Document.xml` dates, `Uid` and `CreatedBy`;
  - the STEP `FILE_NAME` header timestamp and originating system.
- **Semantic fingerprint:** compared within tolerance across versions and architectures. It
  covers:
  - topology counts;
  - volume, area and inertia;
  - bounding box;
  - optionally, the Hausdorff distance between tessellations.

Re-running a job must reproduce the normalized digest on the same image, and the semantic
fingerprint within tolerance on a different image. This check itself runs as a nightly job.

### 3.4 Capability tiers

| Tier | May | May not | Examples |
|---|---|---|---|
| `read` | Query graph and artifact metadata; fetch derived views | Run tools; write anything | `edm-graph` queries |
| `compute` | Run jobs that create new content-addressed artifacts + attestations | Change authored truth; close verifications; baseline | `cad-freecad`, `mesh-gmsh`, `fem-calculix` |
| `propose` | Open a PR / ECR / NCR draft carrying job evidence | Merge, approve, disposition, sign | `edm-propose` |

The gateway grants tiers per principal. An agent's outputs never go past `propose`. A human with
the CM role performs every approval (C03).

### 3.5 Attestation predicate (per job)

The predicate is `edm.dev/job/v1`. The kind-specific predicates (`cad-build`, `mesh`, `solve`,
and so on) extend it.

```json
{ "module": "cad-freecad", "tool": "build_model", "tool_version": "sha256:…(ToolVersion record)",
  "image_digest": "sha256:…", "upstream": {"freecad": "1.1.4", "occt": "7.8.1"},
  "inputs": [{"name": "model_source", "sha256": "…"}, {"name": "params", "sha256": "…"}],
  "outputs": [{"name": "step", "sha256_raw": "…", "sha256_normalized": "…"}],
  "extracted": {"mass": {"value": 1.234, "unit": "KILOGRAM"}},
  "validity": {"passed": true, "checks": ["recompute", "shape_valid"]},
  "requested_by": {"principal": "…", "author_kind": "AGENT",
                   "agent": {"provider": "…", "model": "…", "version": "…", "context_digest": "…"}},
  "intended_use": "IU-CAD-GEOM-01", "accreditation": "ACC-cad-freecad-0.1.0-IU-CAD-GEOM-01",
  "evidence_log": {"seq": 42, "prev_hash": "…"} }
```

## 4. Module #1: `cad-freecad`

### 4.1 Decision: our own thin module; do not fork an existing server

Two servers were evaluated in depth: `neka-nat/freecad-mcp` and
`spkane/freecad-addon-robust-mcp-server` (details in `research/freecad-mcp.md`).

- **Neither can meet §3.2.**
- neka-nat keeps three exec tools that cannot be disabled, and runs a long-lived GUI session.
- spkane's bridge *is* `exec`, with no authentication, and its optional HTTP transport binds
  `0.0.0.0`.

We reuse from them, keeping their MIT notices:

- **From spkane:**
  - its FreeCAD API code templates (reimplemented as direct calls in our entrypoint);
  - its validate and undo-on-invalid logic (our validity gate);
  - its tool taxonomy;
  - its headless `freecadcmd` and Xvfb CI, and its CodeQL, gitleaks and trivy set-up.
- **From neka-nat:**
  - the ccxtools FEM writer flow (deck only);
  - shape serialization.
- **From blwfish (LGPL, used as a concept source only):**
  - its conda-forge Dockerfile pattern;
  - its geometric-fixture regression idea.

### 4.2 Runtime image

- **Base:** conda-forge `freecad` pinned by explicit lockfile (conda-lock or pixi), with `occt`,
  `python` and `vtk` pinned transitively.
  - Start from **FreeCAD 1.1.4**, the final 1.1.x release, which includes the ZipSlip fixes.
  - Track **26.3** (OCCT 8.0.1) as the first planned re-accreditation event.
- **Hardening:**
  - STIG-aligned, with an Iron Bank base where feasible (C15);
  - non-root user and read-only root filesystem;
  - fonts baked in and hashed;
  - a clean `FREECAD_USER_HOME`;
  - no Addon Manager;
  - any addons vendored in.
- **Determinism:** OCCT/TBB and OpenMP forced single-threaded. Spike R2 verifies the exact
  switches.
- **Supply chain:** SBOM and licence inventory generated at build time. The image digest lives
  in `IMAGE_DIGEST.md`.

### 4.3 Models as code (the authored truth)

- The authored model is a reviewed Python module plus `params.yaml`, both kept in git. The model
  file itself is not authored truth.
- The module exposes `build(doc, params) -> BuildReport`. It creates PartDesign Bodies,
  constraint-based Sketches and features.
- A **VarSet or Spreadsheet** is populated from `params.yaml`, and features bind to it through
  expressions. The generated FCStd therefore stays parametric for engineers who open it in the
  FreeCAD GUI.
- **Every feature gets a stable `Label`, and geometry is referenced through datums and LCS**, not
  through `FaceN` or `EdgeN`. This defends against topological naming. It also gives
  `extract_interfaces` stable handles that map to interface ends in `interfaces/*.edm.yaml`.
- **GUI edits are allowed for parameters only.** A `normalize_params` tool reads a GUI-edited
  FCStd and proposes the parameter diff back to the YAML as a PR. Structural edits made in the GUI
  are rejected by policy. Spike R3 tests how practical this is with real engineers.
- **FCStd is a derived artifact:**
  - It is not deterministic.
  - It is untrusted when it arrives from outside.
  - It is parsed only inside the sandbox.

### 4.4 Tool set (v0)

None of these tools accepts inline code.

| Tool | Inputs (by digest) | Outputs | Notes |
|---|---|---|---|
| `build_model` | model source, params | FCStd, STEP (AP214/AP242 geometry), BREP, STL, glTF, `model_report.json` | The core tool; validity gate walks every object (`isValid`, `State`) |
| `recompute_check` | FCStd | validity report | Also used on externally supplied FCStd (sandboxed) |
| `mass_properties` | FCStd or STEP, materials table | mass, volume, area, CoM, inertia tensor (SI units) | Densities come from a pinned `Material` table (C10); FreeCAD mm → m conversion declared |
| `extract_interfaces` | FCStd, interface map | JSON of observed control parameters (datums/LCS placements, hole patterns, mating faces, envelopes) | Feeds `edm plan` (interfaces as code) |
| `interference_check` / `clearance_check` | assembly FCStd | pairs, volumes, minimum distances | Assembly workbench, scripted |
| `diff_models` | two builds | parameter diff + semantic geometric diff | Input to `edm impact` and to PR review |
| `export` | FCStd | STEP, STL/3MF (deflection recorded), glTF, TechDraw SVG/DXF | PDF page export needs offscreen Qt (verify in R1) |
| `render_views` | FCStd | PNGs | **For human review only; never evidence** |
| `fem_prepare` | FCStd, analysis spec (BCs, loads, materials, mesh settings) | gmsh/Netgen mesh, CalculiX `.inp` or Elmer SIF, mesh-quality report | Solving is done by `fem-calculix` / `thermal-elmer`, each separately accredited |
| `run_macro` | macro (allow-listed digest), inputs | declared outputs | Controlled escape hatch; macros are code-reviewed and have their own golden tests |
| `normalize_params` | GUI-edited FCStd | proposed `params.yaml` diff | `propose` tier via `edm-propose` |

### 4.5 Accreditation plan (C07) for `cad-freecad`

Each intended use is paired with a golden suite:

- **IU-CAD-GEOM-01: parametric part geometry and mass properties.**
  - Reference solids with analytic volume, area and inertia: box, cylinder, tube, filleted plate
    and hole patterns.
  - Mass properties of a reference bracket, cross-checked against build123d and an independent
    OCCT build.
  - Tolerances justified by measurement.
- **IU-CAD-IFACE-01: interface feature extraction.**
  - Seeded parts with known bolt-circle radius, hole count and position, datum offsets, and
    mating-face normals.
  - The suite must also catch seeded mismatches.
- **IU-CAD-FEMPREP-01: mesh and deck generation.**
  - The deck is checked structurally against the analysis spec: every BC, load and material
    appears on the right groups.
  - Mesh-quality thresholds are checked.
  - This intended use does *not* cover solution correctness, which belongs to the solver
    module.
- **Re-accreditation triggers:**
  - any change to the FreeCAD, OCCT or image digest;
  - any macro allow-list change, which re-runs that macro's own goldens.

### 4.6 Known limitations, recorded rather than hidden

- **No AP242 PMI and no semantic GD&T.**
  - The MBD/TDP gap is described in `compliance-crosswalk.md` §5.
  - Until it is closed, PMI lives as EDM data on named features and is rendered to TechDraw
    drawings.
- **Topological naming** is mitigated, not solved. The datum-reference convention and the
  validity gate are the defence.
- **Solve-time crashes and hangs in OCCT** are handled by one process per job, hard limits, and
  a typed failure.
- **Release cadence.** FreeCAD's CalVer schedule means about three upgrade decisions a year.
  Automation keeps re-accreditation cheap (C07 step 5).

## 5. Module roadmap

Each module goes through the same contract and its own accreditation.

| # | Module | Why next | Notes |
|---|---|---|---|
| 1 | `cad-freecad` | First CAD module (this document) | FreeCAD 1.1.4 |
| 2 | `mesh-gmsh` | Separates meshing from CAD so each is accredited once | Could be folded into `fem_prepare` initially; split when a second CAD module arrives |
| 3 | `fem-calculix` | Structural: static, modal, buckling, thermomechanical | GPL-2.0; own image; NAFEMS-style goldens |
| 4 | `thermal-elmer` | Conduction and radiation, steady and transient; orbit thermal using AltaVista trajectories and eclipse events | Elmer GPL, with the ElmerSolver library LGPL |
| 5 | `cad-build123d` | Generator-style parts; proves the contract is tool-agnostic | Apache-2.0; same OCCT pin as FreeCAD or a documented difference |
| 6 | `docgen` | ICD/IRS/IDD, VCRM, MIL-STD-3022 and DRD rendering | Reproducible renderers (C17) |
| 7 | `mfg-qif` | QIF plans and results, and inspection extraction | Depends on the PMI decision (R4) |
| — | vendor modules (Ansys, Onshape, Fusion) | Only where licensed | Wrapped to the same contract; their own MCPs sit *behind* our module, never exposed directly |

## 6. The agent side: model-agnostic by construction

- Agents reach modules only through `edm-gateway`. This is AltaVista's `av-gateway` pattern: MCP
  plus gRPC, OIDC, label filtering, deny-by-default and `propose_*` only.
- The gateway records `{provider, model, version, context_digest}` for each call, and checks
  them against the **program model allow-list** (C16). Supplier exclusions such as FASCSA are
  enforced there. The modules are never involved.
- The same MCP tools therefore serve:
  - a local enclave model (secrouter/secllm);
  - a commercial model where the program allows one;
  - a human using a CLI.

## 7. Research spikes (before implementation)

Each spike ends with a short written finding in `docs/edm/research/` and, where it settles a
design decision, a Proposed ADR.

| ID | Question | Method | Exit |
|---|---|---|---|
| R1 | Can every v0 tool run under `freecadcmd` headless with `QT_QPA_PLATFORM=offscreen`, including TechDraw export and Assembly? | Build the conda-locked image; run one script per tool | Per-tool yes/no table; list of GUI-only features |
| R2 | How deterministic is FreeCAD 1.1.4 + OCCT? | Build 20 reference parts ×5 runs × 2 architectures, single- and multi-threaded | Raw vs normalized vs semantic reproducibility rates; the exact single-thread switches |
| R3 | Is "models as code + GUI for parameters only" workable for engineers? | Two engineers build a bracket and a panel with `build(doc, params)`; one edits in the GUI | Time taken, friction points, whether `normalize_params` round-trips |
| R4 | How do we get AP242 PMI? | Evaluate an OCCT XDE writer from our module, FreeCAD PMI proposals #29772/#29797, and a commercial MBD module | Recommendation plus effort estimate; affects C11/C12 |
| R5 | FCStd normalization | Enumerate every non-deterministic field in FCStd and STEP; write a normalizer; test it on R2 output | Normalized digests stable across reruns |
| R6 | Interface extraction robustness | Seeded mismatch suite on datum-referenced and FaceN-referenced models, with a topology change applied | Catch rate; confirms the datum convention |
| R7 | Accreditation evidence shape | Draft IU-CAD-GEOM-01 goldens and one full `Accreditation` record; review against SWE-136 and 5000.61 / MIL-STD-3022 expectations | Record template accepted by a program TA or quality reviewer |
| R8 | Private Sigstore and FIPS in the enclave | Stand up Fulcio, Rekor and TUF with FIPS-mode OpenSSL; sign one job attestation | Working recipe; air-gap notes |
