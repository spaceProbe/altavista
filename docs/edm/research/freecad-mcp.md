# FreeCAD and FreeCAD MCP servers (research note)

**Research date:** 2026-09-28. Part of the EDM plan (`docs/edm-plan.md`).

**Method.** Repositories were shallow-cloned and their source read. Star counts come from GitHub
pages, because the GitHub API was blocked. Claims marked **[unverified]** were not checked against
source or primary documentation.

The user named two candidates:

- **neka-nat/freecad-mcp**
- **spkane/freecad-addon-robust-mcp-server** (see §1.B)

## 1. Candidate servers

### A. neka-nat/freecad-mcp

<https://github.com/neka-nat/freecad-mcp>

**Status**
- MIT licence. About 2.5k stars, 313 forks and 215 commits.
- Version v0.1.25; last commit 2026-09-25. It has a CI test suite.
- Supports FreeCAD 1.0 and 1.1.

**Architecture**
- A GUI addon (the "MCP Addon" workbench) runs a standard-library `SimpleXMLRPCServer` on
  **port 9875** inside the FreeCAD GUI process. Calls are dispatched to the Qt GUI thread.
- A separate `uvx freecad-mcp` process (FastMCP over stdio) connects to it over XML-RPC.

**Tools.** There are 17, each declared with `@tool` in `src/freecad_mcp/server.py`:
- `create_document`, `create_object`, `edit_object`, `delete_object`
- `execute_code`, `execute_code_async`, `execute_code_headless`, `get_async_status`
- `get_view`, `insert_part_from_library`, `get_objects`, `get_object`, `get_parts_list`
- `reload_document`, `list_documents`, `get_rpc_status`, `run_fem_analysis`

There is also one prompt, `asset_creation_strategy`.

**Arbitrary code execution: yes.**
- `execute_code` and `execute_code_async` call `exec(code, _EXEC_NAMESPACE)`. The namespace
  **persists across calls**.
- `execute_code_headless` runs `freecadcmd -c "exec(open(script).read())"`.

**Authentication and binding**
- Binds to `127.0.0.1` by default. It checks the Host header (a defence against DNS rebinding) and
  refuses requests that originate from a browser.
- An opt-in "Remote Connections" setting binds `0.0.0.0` behind an IP/CIDR allow-list.
- An optional bearer/basic token is compared with `hmac.compare_digest`, but it is **off by
  default**, and there is **no TLS**.

**Headless operation**
- Only partly. The addon needs `FreeCADGui`.
- Only `execute_code_headless` works without a GUI, and it takes raw code.

**FEM.** `run_fem_analysis` uses `ObjectsFem` and `femtools.ccxtools.FemToolsCcx`. It supports
CalculiX only and writes to a `mkdtemp` directory.

**Other.**
- `get_view` returns screenshots.
- The parts library loads `.FCStd` files from `Mod/parts_library`, with a guard against path
  traversal.

### B. spkane/freecad-addon-robust-mcp-server

<https://github.com/spkane/freecad-addon-robust-mcp-server>

*Pending source review; this section is filled in by the follow-up evaluation.*

A same-named repository at `LordBoos/freecad-addon-robust-mcp-server` was found in the first
pass. It is MIT-licensed and exposes XML-RPC on port 9875 plus JSON-RPC on 9876. It claims more
than 150 tools, a headless mode and `execute_python`, and documents no authentication. Its
relationship to spkane's repository (fork or upstream) is to be confirmed.

### C. Other servers (reference only)

| Repo | License | Activity | Architecture | Notes |
|---|---|---|---|---|
| blwfish/freecad-mcp | LGPL-2.1 | Very active; v8.2.2 (2026-09-28); 1430 unit + 147 integration tests | Unix socket (0600), stdio bridge, multi-instance; **official Dockerfile** (conda-forge `freecad=1.1.3`, offscreen Qt, headless) | 39 tools (sketch/partdesign/part/assembly/spreadsheet/varset/CAM/mesh/measurement, `geometric_verification`, `fixture_operations`, `execute_python`, macros). No FEM. Checks GitHub for updates daily. SECURITY.md: "single-user local; full OS access by design" |
| contextform/freecad-mcp | **No licence** (unusable) | Stale (2025-08) | Unix socket or `localhost:23456` | npm setup script downloads code at install time |
| bonninr/freecad_mcp | MIT | Abandoned (2025-03) | Raw TCP `localhost:9876`, no auth | Only `send_command` and `run_script` (`exec`) |
| sandraschi/freecad-mcp, cnPauLi/freecad-go-mcp, others | varies | — | — | Not evaluated in depth |

## 2. Assessment against EDM constraints

| Criterion | neka-nat | blwfish | contextform | bonninr |
|---|---|---|---|---|
| Arbitrary Python execution | Yes (3 tools, persistent namespace) | Yes (+ macros, hot-reload) | Yes | Yes (only tools) |
| Auth on RPC | Token optional, off by default | Unix-socket perms / token on Windows | Socket perms | None |
| Network bind | 127.0.0.1; 0.0.0.0 opt-in | Unix socket | Unix socket / localhost | localhost TCP |
| Outbound network | None found | GitHub update check | Install-time fetch | None |
| Headless | Raw exec only | Yes (Docker) | No | No |
| Determinism / provenance | None | Before/after debug logs; stateful | None | None |
| Licence | MIT | LGPL-2.1 | None | MIT |

**The main finding: the design pattern itself does not fit.** Every existing server is built as
an interactive copilot. That design conflicts with EDM requirements in five ways:

1. A long-lived FreeCAD session is mutated call by call.
2. An `exec` escape hatch is always available.
3. State (the document, the persistent namespace, the GUI selection) leaks between calls.
4. Outputs go to temporary paths, and input digests are never recorded.
5. Document content flows straight into the agent's context, which is a prompt-injection path.

This is the opposite of the EDM rule that one tool call is one hermetic, hashed job.

FreeCAD 1.1.4 (2026-09-28) fixed two ZipSlip vulnerabilities that malicious FCStd files could
trigger (<https://github.com/FreeCAD/FreeCAD/releases/tag/1.1.4>). **FCStd files are therefore
untrusted input and are parsed only inside the sandbox.**

**Recommendation (initial).** Write our own thin module on FreeCAD's Python API, and reuse ideas
and code from the others under their licences:

- **From neka-nat (MIT):**
  - the `fem_executor.py` pattern using ccxtools;
  - shape serialization;
  - the headless-subprocess approach;
  - its token and Host-check code.
- **From blwfish (LGPL-2.1):**
  - the Dockerfile pattern;
  - the `fixture_operations` and `geometric_verification` concepts;
  - the assembly and varset handler logic.
  - Any file copied from blwfish becomes LGPL, so each one must be tracked.

The final recommendation is in `docs/edm/tool-modules.md` §5, once the spkane review is in.

## 3. FreeCAD as a platform

### Releases and versioning

- 1.0 shipped Nov 2024 and 1.1 on 2026-03-24. **1.1.4 (2026-09-28) is the last 1.1.x release.**
- FreeCAD is moving to **CalVer with three releases a year** (FEP-0003). **26.3** branches
  2026-09-30 and targets about Nov 2026. It reportedly ships **OCCT 8.0.1**. Backports get about
  4 months.
- **Implication:** pin FreeCAD and OCCT together, and plan for about three upgrade decisions a
  year. Each upgrade is a tool re-accreditation event (see `docs/edm/compliance-crosswalk.md`).
- Sources: <https://github.com/FreeCAD/FreeCAD/discussions/32821>,
  <https://blog.freecad.org/2026/06/26/new-freecad-versioning-scheme-and-development-cycle/>.

### Licence

LGPL-2.1-or-later.

### Headless operation

- Use `freecadcmd script.py`, or `import FreeCAD` from a matching conda-forge Python.
- There is no official Docker image [unverified]. Build one from conda-forge with a conda-lock or
  pixi lockfile, then pin it by digest.
- The feedstock can pin `occt`, `vtk`, `gmsh`, `netgen`, `calculix` and `elmer` builds.

### Modelling

- **Part, PartDesign and Sketcher** are fully scriptable headless. Dress-up features take edge
  references in scripts. The GUI dependency in the MCP servers comes from their interactive
  selection flow, not from FreeCAD.
- **Assembly** (Ondsel solver) is scriptable.
- **Spreadsheet aliases, expressions and `App::VarSet`** are the natural bindings for parameters
  held in YAML.
- **Topological naming** is mitigated since 1.0 (element maps) but not eliminated.

### Drawings, STEP and PMI

- **TechDraw** produces views and dimensions. It has **no semantic GD&T** and no feature control
  frames. PDF page export probably needs offscreen Qt [unverified].
- **STEP** is exported as AP203, AP214, or AP242 geometry only, with **no PMI**. A 3D PMI workbench
  is only a proposal (issues #29772 and #29797).
- **If a program requires MBD/PMI** (the MIL-STD-31000 technical data package, AP242 with PMI),
  **FreeCAD cannot produce it today.** See the compliance crosswalk for how this is handled.

### FEM

- **Meshers:** gmsh and Netgen.
- **Solvers:** CalculiX (the primary one, including thermomechanical and heat transfer), Elmer
  (heat, EM, Joule heating), Z88 and Mystran.
- Everything is scriptable through `ObjectsFem`, `femtools.ccxtools` and `femsolver.run`.
- 1.1 added CalculiX beam, membrane and truss sections, contact and tie on edges, and hex-dominant
  Netgen meshes.
- **CfdOF** (OpenFOAM) downloads dependencies at install time, so it must be baked into the image
  in advance.

### The .FCStd format

- An FCStd file is a ZIP containing `Document.xml`, `GuiDocument.xml`, `*.brp` BRep caches and a
  thumbnail.
- **The format is not deterministic:**
  - `CreationDate`, `LastModifiedDate`, a document `Uid`, `CreatedBy` and `ProgramVersion` change
    between saves;
  - the BReps are rewritten on recompute;
  - the ZIP entry mtimes vary.
- PR #28312, which externalizes the BRep cache and makes `LastModifiedDate` transient, is
  **unmerged**. Its milestone moved to "tbd" on 2026-09-19.
- **Conclusion:** FCStd is a derived output, never the source of truth.

### Recompute reproducibility

- Output depends on:
  - the OCCT version (booleans, fillets, fuzzy tolerances);
  - OCCT/TBB parallelism;
  - platform floating point.
- Expect bit-identical BReps only with the same image on the same CPU architecture. Across
  versions, compare within tolerance: volume, area, inertia, bounding box, and face and edge
  counts.
- STEP `FILE_NAME` headers embed timestamps. Normalize them before hashing.

## 4. Recommended `cad-freecad` module design (input to `docs/edm/tool-modules.md`)

### Runtime

- An OCI image with conda-forge `freecad` pinned. Use 1.1.4 now, and evaluate 26.3 with OCCT 8.0.1
  after 26.3.1.
- Build from a conda-lock explicit file with `QT_QPA_PLATFORM=offscreen`.
- Run as non-root on a read-only root filesystem with `--network=none`, a tmpfs working
  directory, and cgroup limits on memory, CPU and time. Add seccomp, and gVisor where available.
- Pin a clean `HOME` / `FREECAD_USER_HOME` for each job.
- Record `FreeCAD.Version()`, `Part.OCC_VERSION`, the image digest and the lockfile hash in every
  attestation.
- Force OCCT/TBB single-threaded [verify the switch].

### Execution model

- The MCP server validates arguments and writes `job.json`: the tool, parameters, input sha256s
  and the image digest.
- The runner starts a **fresh** `freecadcmd` container for each job, hashes the outputs, and emits
  an attestation.
- There is **no persistent FreeCAD process and no RPC port.**

### Curated tools

There is no `exec` tool by default. The curated tools are:

| Tool | Output |
|---|---|
| `build_model(model@sha256, params@sha256)` | FCStd, STEP, BREP, STL, glTF, `model_report.json` |
| `recompute_check` | Per-object validity and state flags; the job fails on any invalid object |
| `mass_properties` | Volume, area, centre of mass, inertia; density from a pinned materials file |
| `extract_interfaces` | Named datums/LCS placements, hole patterns, mating faces (normal, centroid), bbox and keep-out envelopes. JSON consumed by `edm plan` |
| `interference_check`, `clearance_check` | Assembly spatial queries |
| `export` | STEP (geometry only), STL/3MF with deflection recorded, glTF, TechDraw SVG/DXF |
| `render_views` | Screenshots for human review, **never evidence** |
| `diff_models(a, b)` | Parameter diff plus geometric diff within tolerance (topology counts, volume, bbox, Hausdorff) |
| `fem_prepare` | CalculiX `.inp` / Elmer SIF, mesh and mesh-quality report. **Solving happens in separate solver modules** |
| `run_macro(macro@sha256)` | Runs only macros whose digests are on a human-approved allow-list (merged by PR). It takes no inline code |

### Models as code

- The source of truth is a Python module with a `build(doc, params)` function plus a
  `params.yaml`.
- The module binds a Spreadsheet or VarSet from the YAML, so the FCStd keeps live expressions for
  engineers who work in the GUI.
- Every feature gets a stable `Label`. Geometry is referenced through datums and LCS rather than
  `FaceN`. This is the toponaming defence, and it gives interface extraction stable handles.
- GUI edits outside the parameters are disallowed by policy. A normalizer can write parameter
  edits back to the YAML.

### FEM hand-off

- `cad-freecad` writes the solver decks.
- Separate `fem-calculix` and `fem-elmer` images solve and post-process them.
- This keeps the GPL solvers in their own images, lets solvers be qualified independently of the
  CAD version, and hashes solves separately.

### FreeCAD versus build123d/CadQuery (Apache-2.0)

| | FreeCAD | build123d/CadQuery |
|---|---|---|
| Code-first | Secondary to the GUI | Native |
| Determinism | Stateful document engine | Pure functions |
| GUI for engineers | Yes: Sketcher, Assembly, TechDraw, FEM pre-processing, CAM | No |
| Containers | Qt/Coin dependencies | Small, no Qt |

Both are feasible modules behind the same contract. Both use OCCT, so pin the same OCCT version or
BReps will differ.

## 5. Pitfalls

- **GUI coupling.** Some addons import `FreeCADGui` at import time. TechDraw page export,
  screenshots, and some Assembly and Draft paths need offscreen Qt. Fonts must be baked into the
  image and hashed.
- **Silent recompute failures** leave objects `Invalid` or `Touched`, or produce null shapes. Walk
  every object and fail the job.
- **OCCT segfaults and hangs** in booleans, sweeps and fillets. Run one process per job, with a
  hard timeout and a memory cgroup.
- **User configuration leaks** in through `user.cfg`, Mod and macro paths.
- **Addon Manager downloads.** Vendor every addon, and pre-bake CfdOF's OpenFOAM, cfMesh and HiSA.
- **Hashing.** Normalize STEP headers, ZIP mtimes, and the dates and UIDs in `Document.xml`. Record
  both raw and normalized digests.

### Solver and library licences

| Component | Licence |
|---|---|
| CalculiX | GPL-2.0 |
| gmsh | GPL-2.0+ with exception |
| Elmer | GPL, with the ElmerSolver library LGPL |
| Z88OS | GPL [unverified] |
| Mystran | MIT |
| Netgen | LGPL-2.1 |
| OpenFOAM | GPL-3 |
| OCCT | LGPL-2.1 with exception |

- Calling solvers as separate executables through files is mere aggregation.
- **Distributing** images, including to government customers or subcontractors, triggers GPL
  source-offer and notice obligations. Keep a per-image SBOM and a source mirror for air-gapped
  delivery.
- Have counsel confirm this. It is not legal advice.

## Sources

- **Code reviewed:**
  - neka-nat: `src/freecad_mcp/server.py`, `addon/FreeCADMCP/rpc_server/*`, `docs/configuration.md`,
    `docs/execution.md`
  - blwfish: `TOOLS.md`, `SECURITY.md`, `Dockerfile`, `AICopilot/freecad_mcp_handler.py`
  - contextform: `socket_server.py`
  - bonninr: `freecad_mcp.py`
- **Web:**
  - <https://github.com/FreeCAD/FreeCAD/releases>
  - <https://blog.freecad.org/2026/03/25/freecad-version-1-1-released/>
  - <https://blog.freecad.org/2025/09/09/what-is-new-in-fem-for-freecad-1-1/>
  - <https://anaconda.org/conda-forge/freecad>
  - <https://github.com/FreeCAD/FreeCAD/pull/28312>
  - <https://github.com/freecad/freecad/issues/29797>
  - <https://gmsh.info/LICENSE.txt>
  - <https://www.elmerfem.org/blog/license/>
