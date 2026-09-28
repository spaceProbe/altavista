# Engineering Data Model (EDM) — Implementation Plan (v0)

A common, code-first engineering data model that tracks a program from ideation through
requirements, design, implementation, verification/validation and manufacturing. Interfaces
are declared and reconciled like infrastructure as code. Large artifacts are content-addressed
and attested. AI agents work through MCP tool servers and can only propose.

Status: draft for review. It is a sibling of spoore (the tracking CDM) and AltaVista
(the simulation CDM, `altavista.v1`), and it reuses their conventions deliberately:

- one data model, declared and hashed;
- the artifact is the evidence;
- refusals are typed and early;
- models propose, humans authorize.

The working name is "EDM" and the proto package is `edm.v1`. Both are placeholders until
ADR-E000.

**Revision 2026-09-28 (second).** The user answered four scoping questions:

- all engineering tools are modular, starting with FreeCAD over MCP;
- the program must meet DoD **and** NASA expectations;
- it is greenfield, with no incumbent tool;
- the current phase is research and planning only.

The companion documents are:

| Document | Contents |
|---|---|
| `docs/edm/compliance-crosswalk.md` | DoD + NASA obligations merged into EDM capabilities C00–C19, the unified analysis credibility record, the tool accreditation lifecycle, known gaps, hosting implications |
| `docs/edm/tool-modules.md` | The module and job contract every tool follows; `cad-freecad` as module #1; module roadmap; research spikes R1–R8 |
| `docs/edm/research/freecad-mcp.md` | neka-nat, spkane and other FreeCAD MCP servers evaluated from source; FreeCAD as a platform |
| `docs/edm/research/dod-requirements.md` | DoD sources → EDM obligations |
| `docs/edm/research/nasa-requirements.md` | NASA sources → EDM obligations |

Where this plan and those documents differ, those documents are newer. §7.2, §11 and §12 below
have been amended to match.

---

## 1. Objectives (restated as requirements)

**E1 — Full lifecycle trace.** Every stakeholder need and system objective is decomposed
into requirements. Each requirement has one or more design elements that satisfy it, the
implementation artifacts that realize those elements, and one or more verification
activities whose results close it on the as-built system. Orphans in either direction are
a computable defect, not a review finding.

**E2 — Interfaces as code.** Every system interface is declared in version-controlled text:
mechanical, electrical, data, thermal, fluid and software. Interfaces are owned,
reviewed, versioned and **reconciled** against the design and the as-built hardware. The
model is a `plan`/`apply` loop: the declared state is compared with observed state, and
drift is reported.

**E3 — Large-artifact integrity.** CAD, meshes, simulation results, test data, drawings,
firmware images and inspection reports are tracked by content digest. Each one has signed
provenance recording what produced it, from which inputs and with which tool at which
version. Evidence whose inputs have changed is **stale** automatically.

**E4 — Modular AI integration.** Agents drive CAD, meshing and mechanical/thermal analysis
through MCP servers. Every tool call is a hashed, reproducible job that emits an
attestation. Agents open change proposals, and humans (with CODEOWNERS-style interface
owners) accept them.

**E5 — Standards interoperability.** The model imports and exports:

- SysML v2 (text and API) for the system model;
- ReqIF for requirements with customers and suppliers;
- STEP AP242 for geometry and PMI;
- QIF for inspection;
- CycloneDX HBOM/MBOM for bills of materials;
- OSLC for trace links into commercial tools.

The EDM never becomes a walled garden.

Cross-cutting requirements:

- **Phase boundaries are configuration, not rewrites.** AltaVista's rule applies here too.
- **Everything that can affect a verification verdict is a declared, hashed artifact.**
- **Closure is a query, not a spreadsheet.**

---

## 2. What exists, and what to adopt vs build

The research was done 2026-09-28. Items marked † could not be verified directly (see §12
Q-E9).

### 2.1 OpenMBEE

- **Legacy line (SysML v1): MMS 3/4, View Editor and the Cameo MDK.** Docker images for it
  are marked deprecated. Do not adopt it for a new program.
- **Flexo MMS, the active line.** It is a set of microservices over an RDF quadstore
  (SPARQL 1.1):
  - **Layer 1** provides version-controlled CRUD with git-like branches, commits,
    diff and merge.
  - Around it sit **flexo-mms-sysmlv2**, which serves the OMG *Systems Modeling API &
    Services 1.0* REST binding (Kotlin, Apache-2.0), plus GraphQL, auth and a
    Python client.
  - **flexo-mms-sysmlv2-mcp** is an MCP server with about 35 tools and a read-only mode.
  - All of these were active in Sep 2026 and all are **pre-1.0**.
  - https://github.com/Open-MBEE/flexo-mms-layer1-service,
    https://github.com/Open-MBEE/flexo-mms-sysmlv2,
    https://github.com/Open-MBEE/flexo-mms-sysmlv2-mcp
- **Fit.** Flexo is the natural *query and collaboration server* for the model. Its
  RDF substrate lets us link requirements, CAD, analysis and manufacturing data in one graph
  without inventing a database. It is **not** the authored source of truth; git is
  (ADR-E001 below). We pin versions and contribute upstream, the same way AltaVista does
  with spoore and secdeploy.

### 2.2 SysML v2 / KerML

- **Status.** OMG adopted SysML v2.0, KerML 1.0 and API & Services 1.0 in mid-2025. The
  pilot implementation is already tracking 2.1 Beta (EPL-2.0, with a Jupyter kernel).
- **Why it fits "interfaces as code".** It has a normative **textual notation** that diffs
  in git. It has first-class `port def`, `interface def`, `connection def` and
  `flow def`, plus ISQ/SI quantity libraries. It also has `requirement def`,
  `satisfy`, `verify`, `verification def` and `analysis def`.
- **Tools:**
  - **Pilot implementation:** the reference parser and validator for CI.
  - **Sensmetry Syside:** a free VS Code editor. Automator (its Python API) is paid.
  - **Sysand:** an open-source SysML v2 package manager.
  - **SysON (Obeo):** an open-source web modeler that exchanges models with Capella.
  - **Gaphor:** SysML v1 only for now.
- **Caveats:**
  - Results are not yet identical across tools.
  - Semantic diff and merge by element ID is immature.
  - No certification authority has yet accepted SysML-v2-based evidence directly. We
    therefore *generate* traditional trace matrices and VCRMs from the graph.

### 2.3 Interchange and reference standards

| Concern | Standard | Use in EDM |
|---|---|---|
| Trace links across tools | OSLC Core 3.0, RM 2.1, QM, CM, AM | Link vocabulary and the export surface to DOORS, Polarion and Jama |
| Requirements exchange | ReqIF (OMG) | Customer and supplier boundary; StrictDoc round-trips it |
| Geometry and PMI | STEP AP242 Ed. 4 (ISO 10303-242:2025) | Canonical neutral CAD artifact; PMI drives inspection |
| Simulation metadata | STEP AP243 MoSSEC (ISO 10303-243:2021) | **Borrow its concepts** for studies, model credibility and validation |
| Inspection | QIF 3.0 (ISO 23952) | Measurement plans and as-inspected results close dimensional requirements |
| BOM and hardware supply chain | CycloneDX 1.7 HBOM/MBOM (ECMA-424) | EBOM/MBOM and supplier provenance, the hardware analogue of the SBOMs AltaVista already emits |
| Software and bus interfaces | OpenAPI 3.x, AsyncAPI 3.x†, protobuf | **Generated** from interface definitions, never authored twice |
| Artifact provenance | in-toto attestations, SLSA, Sigstore/cosign | Engineering predicate types (§5.3) |

### 2.4 Artifact storage

- **OCI registry + ORAS:**
  - Every artifact is content-addressed by sha256.
  - Signatures, SBOMs and attestations attach through the OCI 1.1 *referrers* API.
  - Air-gapped mirroring is a solved problem.
- **lakeFS** provides git semantics over S3 for large mutable result lakes (CFD/FEM
  sweeps).
- **DVC** is kept only for in-repo pipelines, if at all.
- **git-lfs** is avoided for anything over a few GB.
- AltaVista's `av-store` already implements the core of this: content-addressed MinIO at
  `<prefix>/<hh>/<hh>/<sha256>`, label check before fetch, size and sha256 verified after,
  and the `AssetRef` claim-check. The EDM reuses it rather than starting over.

### 2.5 Requirements authoring

- **StrictDoc** (`.sdoc` text in git) supports ReqIF and has a DO-178C trace note.
- **sphinx-needs** is the docs-as-code alternative.
- **Doorstop** is barely maintained.
- **Jama** is the commercial incumbent in aerospace and has a good REST API.
- **Valispace** is in limbo after the Altium and Renesas acquisitions.
- **Recommendation:**
  - Model-level requirements live in SysML v2 (`requirement def`).
  - Document-style and derived requirements that engineers want to write as prose live in
    StrictDoc.
  - One UID space spans both, enforced by the linker (§4).

### 2.6 MCP servers for engineering tools (Sep 2026)

| Domain | Official / vendor | Community | Assessment |
|---|---|---|---|
| SysML v2 | OpenMBEE flexo-mms-sysmlv2-mcp | jgs-sysmlv2-api-mcp (staged commits), mcp-syson, sysml-v2-lsp | Nascent; adopt the Flexo server behind our gateway |
| CAD | Onshape FeatureScript MCP (PTC), Autodesk Fusion MCP / Fusion Data MCP | freecad-mcp (32 tools), build123d-mcp, CadQuery servers, SolidWorks (community only) | **Code-CAD (build123d/CadQuery on OCCT) is the most agent-friendly**: the model *is* diffable, reviewable code |
| FEA/CFD/thermal | **PyAnsys MCP** (Mechanical, Fluent, CFX; needs a licence) | mcp-calculix (gmsh + CalculiX linear static), cloudhpc-mcp, OpenFOAM, FEniCSx/NGSolve multi-backend | Open-solver servers are demo-grade: no V&V pedigree, and face selection by bounding box |

- The awesome-ai-cae list indexes these: https://github.com/kimimgo/awesome-ai-cae
- **Conclusion:**
  - Adopt the vendor MCPs where the program already holds licences.
  - **Build** hardened, containerized, attesting MCP wrappers for the open toolchain:
    - build123d for CAD;
    - gmsh for meshing;
    - CalculiX for structures;
    - Elmer for thermal and multiphysics;
    - OpenFOAM for CFD.

### 2.7 Programmatic references

- **DoDI 5000.97 (Digital Engineering, Dec 2023).** It requires an *authoritative source
  of truth* (ASOT). In the EDM that is git (authored truth) plus the evidence store, with
  the query graph derived from them.
- **NASA NPR 7123.1D and SE Handbook Rev 2.** V-model; verification by Test, Analysis,
  Inspection and Demonstration; a VCRM; product validation kept distinct from
  verification.
- **DO-178C / DO-254 / ARP4754B / ARP4761A.** These require bidirectional traces. Safety
  assessment derives requirements. Rigor is scaled by development assurance level (DAL).
  The EDM carries a DAL attribute on requirements, and the closure gate scales its
  checks by DAL.

### 2.8 Decision summary

**Adopt:**
- SysML v2 text as the authored truth for architecture, interfaces and model requirements.
- The SysML v2 API as the exchange contract.
- Flexo MMS as the query and collaboration server.
- StrictDoc and ReqIF for prose requirements and at external boundaries.
- OCI + ORAS + cosign + in-toto for artifacts.
- lakeFS for result lakes.
- AP242, QIF and CycloneDX HBOM/MBOM.

**Reuse from AltaVista:**
- `Provenance`, `AssetRef`, `Label`, `Unit` and time conventions.
- `av-store` and `av-catalog`.
- The hash-chained evidence log.
- The `av-gateway` MCP pattern and the `av-command` propose/authorize ledger.
- The control-matrix and evidence-bundle mechanics.

**Build:**
- The `edm.v1` records that SysML v2 does not cover well: evidence, baselines,
  manufacturing, as-built units, and non-conformance reports (NCRs).
- The linker and graph compiler.
- The engineering in-toto predicates.
- The closure and staleness engine.
- The interface reconciler (`plan`/`apply`).
- The hardened MCP tool servers.

---

## 3. Architecture

```
 authored truth (git, PR-reviewed)                 derived (rebuildable, never edited)
 ─────────────────────────────────                 ───────────────────────────────────
 model/*.sysml     architecture, interfaces,  ─┐
                   requirement defs, verif.    │   edm-lint ─▶ edm-link ─▶ engineering graph
 reqs/*.sdoc       prose/derived requirements  ├─▶ (parse,     (UIDs,      ├─▶ Flexo MMS (RDF, SysML v2 API,
 edm/*.edm.yaml    evidence plans, baselines,  │    units,      typed       │     SPARQL, GraphQL, MCP read)
                   mfg records (edm.v1 mirror) │    schema)     links)      ├─▶ closure report / VCRM / ICDs
 cad/*.py          code-CAD (build123d)        │                            └─▶ interface plan (drift)
 analysis/*.yaml   analysis job specs         ─┘
                                                        ▲ digests only
 artifact store (content-addressed) ────────────────────┘
   OCI registry / MinIO (av-store): STEP, meshes, VTU/HDF5 results, test data, QIF, firmware
   each blob ← cosign signature + in-toto attestations (OCI referrers)
   lakeFS for large mutable result lakes

 tool plane (MCP)                                      authority
   edm-gateway (MCP, deny-by-default, OIDC, labels) ─▶ propose-only: opens a change (PR) or a Job
   job runner: pinned-digest containers ─ build123d │ gmsh │ CalculiX │ Elmer │ OpenFOAM │ PyAnsys
   every job: hashed inputs → outputs + attestation → evidence log (hash-chained)
```

**Three planes, the same split as AltaVista:**

1. **Authored truth** is git. It is small, textual and reviewed. Pull requests are the
   change-control process and branch protection is the configuration control board (CCB).
   A **baseline** is a signed git tag plus a manifest of artifact digests (§6).
2. **Derived state** is the engineering graph, the closure status, the ICDs and the
   VCRM. Every CI run rebuilds it from the authored truth plus artifact digests. Flexo
   MMS hosts it for query. A graph that cannot be rebuilt from git plus the store is a
   defect.
3. **Evidence** consists of the artifacts plus attestations. It is immutable and
   content-addressed. It is referenced from the authored truth only by digest.

**Why git rather than Flexo as the authoritative source (ADR-E001 candidate):**

- Git gives us PR review, CODEOWNERS for interfaces, signed commits, offline and
  air-gapped operation, and one mechanism shared with the code.
- Flexo gives us a graph query surface and SysML v2 API conformance for tools that speak
  it.
- The falsifier: if engineers' primary authoring moves to a graphical SysML v2 tool
  (SysON, Cameo 2026x) that can only round-trip through the API, the arrow reverses. In
  that case Flexo becomes the authority and git holds exports.

**Why RDF for the graph:**

- Requirements, CAD, analysis and manufacturing records come from different schemas and
  vendors. A triple store unifies them without migrations.
- OSLC is itself RDF.
- Flexo already provides it versioned.

A property graph (Neo4j) was considered. It was rejected because it has no standard
interchange and would duplicate Flexo.

---

## 4. The data model

### 4.1 Core rule

- **SysML v2 owns the *system model*:** parts, ports, interfaces, requirements, and
  `satisfy`/`verify` relationships.
- **`edm.v1` owns what SysML v2 does not model well:**
  - evidence and attestations;
  - baselines;
  - artifacts;
  - analysis jobs;
  - manufacturing (MBOM, process plans, serialized units, inspections, NCRs);
  - the explicit trace-link ledger for anything outside the SysML model.
- **`edm.v1` imports `altavista.v1` core types rather than redefining them:** `Provenance`
  (including `AuthorKind.AGENT`), `AssetRef`, `Label`, `Unit` and TAI-nanosecond time.
  - It also follows AltaVista's schema rules: strict `buf breaking`, `v2` a new package,
    and the canonical hash defined as SHA-256 over the deterministic encoding with the
    `hash` field cleared.
  - YAML mirrors the proto field for field (as `drms/*.drm.yaml` does).

### 4.2 Identity

- Every record has a stable **UID**: `<kind>:<program>:<local>`, for example
  `req:VX1:THM-012`, `iface:VX1:BUS-PL-01` or `unit:VX1:SN0042`.
- **SysML elements** keep their SysML element ID. The UID is carried as a short name or
  metadata annotation, and the linker enforces a bijection between the two.
- **Versions:**
  - A record's *version* is its content hash.
  - Its *revision* is a human label (A, B, C) assigned only at baseline.
- **External IDs are `Alias{scheme, value}`**, the AltaVista pattern. Examples: DOORS
  object ID, Jama ID, PLM part number, supplier lot.

### 4.3 Entities along the lifecycle

| Phase | Entity | Source of truth | Key fields |
|---|---|---|---|
| Ideation | `Need` / `Objective` | `.sysml` (concern/requirement) or `.sdoc` | stakeholder, rationale, MOE link |
| Requirements | `Requirement` | `.sysml` `requirement def` or `.sdoc` | UID, text, **constraint expression with units**, verification method(s) {T,A,I,D}, DAL/criticality, parent(s), status |
| Architecture | `Part`, `Port`, `InterfaceDef`, `Connection` | `.sysml` | typed ports, flow items, quantities with units, owner |
| Interfaces | `Interface` (instance) | `.sysml` + `interfaces/*.edm.yaml` | two or more ends, owner of each end, **control parameters** with tolerances, generated ICD |
| Design | `DesignElement` → `Artifact` | `.sysml` part + `cad/*.py` / STEP digest | satisfies[] requirements, realizes[] parts |
| Analysis | `AnalysisCase` → `Job` → `Result` | `.sysml` `analysis def` + `analysis/*.yaml` | inputs by digest, solver and container digest, outputs, **extracted scalars with units**, credibility (MoSSEC-style) |
| Verification | `VerificationCase` → `VerificationResult` | `.sysml` `verification def` + evidence | method, procedure, pass criterion, verdict, margin, configuration verified |
| Validation | `ValidationCase` | as above, against `Need` | "right system": operational scenario, AltaVista DRM or spoore golden |
| Manufacturing | `PartNumber`, `MBOM`, `ProcessPlan`, `SerializedUnit`, `Inspection`, `NCR` | `edm.v1` YAML + QIF/AP242 artifacts | as-designed→as-planned→as-built; lot/serial genealogy |
| Configuration | `Baseline` | signed tag + manifest | SRR/PDR/CDR/TRR/FCA/PCA milestone, digests of everything in scope |

**Validation vs verification.**
- Verification closes a `Requirement` ("built it right").
- Validation closes a `Need` against the final system ("built the right thing").
- AltaVista's `DesignReferenceMission`, with its `Objective`/`ScoreResult`, is
  *already* a validation case runner. The EDM links to a DRM by hash and consumes its
  `RunProducts` as evidence.
- spoore golden scorecards play the same role for the tracker.

### 4.4 Trace links

- Links are typed, directed and carry their own provenance.
- The vocabulary is the OSLC and SysML v2 set:
  - `derivedFrom`, `refines` (requirement ↔ requirement);
  - `satisfies` (design → requirement);
  - `realizes` (artifact → design element);
  - `allocatedTo` (requirement/function → part);
  - `verifies` / `validates` (case → requirement or need);
  - `producedBy` (artifact → job);
  - `consumes` (job → artifact);
  - `conformsTo` (unit → baseline);
  - `dispositions` (NCR → unit/requirement).
- Inside the SysML model, links are native SysML relationships.
- Across files and tools they live in the `edm.v1` link ledger.
- The linker merges both into one graph and **refuses** dangling targets and duplicate
  UIDs at load (a typed error, never a warning).

### 4.5 Proto sketch (`proto/edm/v1/`, illustrative only, not yet committed)

```proto
syntax = "proto3";
package edm.v1;
import "altavista/v1/core.proto";    // Provenance, Unit
import "altavista/v1/entity.proto";  // AssetRef, Alias
import "altavista/v1/envelope.proto";// Label

enum VerificationMethod { VM_UNSPECIFIED = 0; VM_TEST = 1; VM_ANALYSIS = 2;
                          VM_INSPECTION = 3; VM_DEMONSTRATION = 4; }

message Quantity { double value = 1; altavista.v1.Unit unit = 2; }

message Constraint {                       // machine-checkable part of a requirement
  string expression = 1;                   // same grammar family as altavista Objective
  Quantity limit = 2; Quantity tolerance = 3;
  string comparator = 4;                   // "<=", ">=", "within"
}

message Requirement {
  string uid = 1; string title = 2; string text = 3;
  repeated Constraint constraints = 4;
  repeated VerificationMethod methods = 5;
  string criticality = 6;                  // DAL A–E or program scale
  repeated string parent_uids = 7;
  string sysml_element_id = 8;
  repeated altavista.v1.Alias aliases = 9; // DOORS/Jama/ReqIF ids
  altavista.v1.Label label = 10;
  altavista.v1.Provenance provenance = 11;
  string hash = 15;
}

message TraceLink { string from_uid = 1; string to_uid = 2; string kind = 3;
                    altavista.v1.Provenance provenance = 4; }

message InterfaceEnd { string part_uid = 1; string port = 2; string owner = 3; }
message ControlParameter { string name = 1; Quantity nominal = 2;
                           Quantity tol_minus = 3; Quantity tol_plus = 4;
                           string observed_by = 5; } // extractor id (§5.2)
message Interface {
  string uid = 1; string kind = 2;         // mechanical|electrical|data|thermal|fluid|software
  repeated InterfaceEnd ends = 3;          // ≥2; each end has an owner who must approve
  repeated ControlParameter parameters = 4;
  string schema_ref = 5;                   // proto/AsyncAPI/OpenAPI for data interfaces
  repeated altavista.v1.AssetRef icd_artifacts = 6;
  altavista.v1.Provenance provenance = 7; string hash = 15;
}

message Job {                              // one tool invocation; the unit of reproducibility
  string job_id = 1; string tool = 2;      // "calculix", "build123d", "elmer"...
  string container_digest = 3;             // sha256 of the pinned image
  string spec_hash = 4;                    // canonical job spec
  repeated altavista.v1.AssetRef inputs = 5;
  repeated altavista.v1.AssetRef outputs = 6;
  map<string, Quantity> extracted = 7;     // scalars that verdicts are computed from
  altavista.v1.Provenance provenance = 8;  // AuthorKind.AGENT when an agent launched it
}

message VerificationResult {
  string case_uid = 1; repeated string requirement_uids = 2;
  string verdict = 3;                      // PASS|FAIL|INCONCLUSIVE
  Quantity margin = 4;
  repeated string evidence_job_ids = 5;
  repeated altavista.v1.AssetRef evidence = 6;
  string baseline_id = 7;                  // configuration that was verified
  repeated string input_digests = 8;       // staleness key (§6.2)
  altavista.v1.Provenance provenance = 9; string hash = 15;
}

message Baseline { string id = 1; string milestone = 2; string git_tag = 3;
                   string git_commit = 4; map<string, string> artifact_digests = 5;
                   string hash = 15; }

message SerializedUnit { string serial = 1; string part_number = 2; string baseline_id = 3;
                         repeated string child_serials = 4;   // genealogy
                         repeated altavista.v1.AssetRef inspections = 5; // QIF results
                         repeated string open_ncrs = 6; string hash = 15; }
```

---

## 5. Interfaces as code

### 5.1 Declaration and ownership

- Interfaces are declared in `.sysml`, using `interface def` and `port def` with typed flows
  and quantities.
- Instance-level control parameters live in `interfaces/<uid>.edm.yaml`. Examples: bolt
  pattern, connector pinout, heat-flux budget, message schema.
- **Each interface end names an owner.** CODEOWNERS is generated from the owners, so a
  change to an interface needs approval from **every** end's owner. That is the digital
  version of an ICD signature block.
- Data interfaces reference a `schema_ref`. This can be AltaVista's own `Port.schema`
  (CDM type or framing name such as `ccsds.spp`), a proto, or an AsyncAPI document.
  OpenAPI/AsyncAPI/proto are **generated** from the SysML interface where possible,
  never maintained in parallel.
- ICD documents (PDF/HTML) are **build outputs**. They are generated, hashed, and attached
  to the baseline.

### 5.2 `plan` / `apply`: reconciliation, the IaC core

The comparison with Terraform and Kubernetes:

| IaC concept | EDM equivalent |
|---|---|
| Desired state (HCL/manifests) | Declared interfaces, control parameters and tolerances |
| Provider reads actual state | **Extractors** read observed state from artifacts |
| `terraform plan` → diff | `edm plan` → interface drift report: which parameters are out of tolerance or unobserved, per end |
| `apply` | For design-side drift, an agent or engineer proposes a CAD/model change (PR). Hardware-side drift raises an NCR |
| State file | The digest manifest of the current baseline |
| Policy as code (OPA/Rego) | Rego rules on the graph, the same engine AltaVista's `av-command` uses (`regorus`): "no Interface without two owners", "DAL-A requirement needs ≥2 independent verification methods" |
| Drift detection in CI | `edm plan` runs on every PR and nightly; drift blocks merge to a baseline branch |

**Extractors** are small, versioned, containerized readers. Each takes an artifact digest
and returns observed `Quantity` values:

- **STEP/AP242:** hole positions, datum features and envelope, using OCCT.
- **Code-CAD:** evaluate the build123d model and query named features, with no STEP round-trip.
- **ECAD netlist:** pinout.
- **Thermal result:** interface temperature and heat flux.
- **QIF:** as-measured dimensions.
- **Telemetry capture:** observed message rate and schema, via AltaVista `port_traffic_hash`.

Each extractor run is itself a `Job`, so observed values carry provenance.

The same interface is therefore checked **three times across the lifecycle**:

1. declared vs design (CAD/analysis extractors);
2. declared vs as-built (QIF/inspection extractors);
3. declared vs in-operation (telemetry extractors).

That is the "every interface defined and tracked" objective made executable.

### 5.3 Engineering attestation predicates

These are in-toto Statement v1 predicates. The subject is the output digest(s). Each is
signed with cosign (keyless OIDC where there is network, a program key held in an
HSM when air-gapped).

- `edm.dev/cad-build/v1`: source commit, CAD kernel version, parameters, output STEP digest,
  mass properties.
- `edm.dev/mesh/v1`: geometry digest, mesher and version, size field, quality stats.
- `edm.dev/solve/v1`: mesh digest, solver and version, container digest, boundary-condition
  spec hash, convergence record, extracted scalars.
- `edm.dev/verification/v1`: case UID, requirement UIDs, verdict, margin, input digests,
  baseline.
- `edm.dev/inspection/v1`: serial, QIF plan digest, QIF results digest, CMM/instrument
  calibration ID.
- `edm.dev/test-run/v1`: procedure digest, test article serial, facility and instrument
  calibrations, raw-data digests.

Attestations attach to artifacts through OCI referrers and are also appended to the
hash-chained evidence log. That log uses the same `prev_hash` chain as AltaVista's
`evidence.py` / `evidence.rs`. No established use of in-toto for CAE and hardware was
found, which makes this a genuine contribution and a paper seed (§10).

---

## 6. Closure, staleness and baselines

### 6.1 Closure is a query

A requirement is **closed at baseline B** iff all of the following hold:

- every required method has a `VerificationResult` with verdict PASS;
- each such result's `baseline_id` is B, or is compatible with B (below);
- none of its input digests is stale relative to B;
- no open NCR dispositions against it.

A need is **validated** iff its validation cases pass on a `SerializedUnit` (or on a
qualification unit, by rule) conforming to B.

The outputs, all generated and hashed:

- VCRM
- bidirectional trace matrix, including the DO-178C/ARP4754B views
- orphan report
- closure burndown

### 6.2 Staleness propagates through the digest DAG

- Every `Job` records its input digests. Changing a CAD source changes the STEP digest,
  which invalidates the mesh, then the solve, then the verification result, then closure.
- `edm impact <uid|digest>` prints the blast radius **before** a change merges. This is the
  engineering equivalent of a Terraform plan's "N to change".
- A stale result is *open*, never silently carried forward.
- **Compatibility rule.** A result may be carried to a new baseline only if its input
  digests are unchanged. That rule is the whole mechanism; there is no manual "similarity"
  claim without an ADR-style justification recorded as a waiver record.

### 6.3 Baselines and phase gates

- A baseline is a signed git tag plus `Baseline` manifest: the digests of every artifact
  in scope and the closure snapshot.
- Milestone reviews (SRR, PDR, CDR, TRR, SAR, and FCA/PCA) are **queries with entry
  criteria written as Rego**. Examples:
  - PDR requires all L2 requirements allocated.
  - CDR requires all interfaces with a zero `edm plan` diff on design.
  - PCA requires as-built units to conform to the baseline with all NCRs dispositioned.
- The phase changes the query, never the data model.

### 6.4 Manufacturing thread

- **EBOM → MBOM.**
  - The EBOM is generated from the SysML part tree plus CAD.
  - The MBOM is authored as `edm.v1` YAML. It adds process parts, kits and consumables.
  - Both are exported as CycloneDX HBOM/MBOM.
- **Process plans** reference AP242 PMI. Inspection plans are QIF MeasurementPlan documents
  generated from PMI.
- **As-built.** A `SerializedUnit` carries genealogy (child serials, material lots),
  QIF results, test-run attestations and NCRs.
- An **NCR** is a record with a disposition: use-as-is, rework, repair or scrap.
  - A use-as-is disposition that affects a requirement must link a new analysis
    `VerificationResult` showing margin on the as-built values. The as-built CAD or
    parameters come from the extractor, and the analysis re-runs automatically through
    the tool plane.
  - Closing the loop this way is one of the highest-value demonstrations.

---

## 7. The AI and MCP tool plane

### 7.1 Principles, inherited from AltaVista ADR-004 and `aiplane-plan.md`

- **Models propose, humans authorize.** An agent can:
  - read the graph (label-filtered);
  - run jobs in the sandbox;
  - open a change proposal (a PR or a PROPOSED record).

  It can never merge, baseline, disposition an NCR or sign a verification verdict.
- **Every agent action is evidence.** Each tool call becomes a `Job` with
  `Provenance.author_kind = AGENT`, the principal, the model and the prompt hash, and it
  produces an attestation. "What did the model see and do" is answerable from the
  evidence log.
- **Deny-by-default tool allow-lists, OIDC on every caller, clearance from the token.** This
  is the `av-gateway` MCP pattern, generalized.
- **Deterministic, pinned tools.** Every tool server runs in a container pinned by digest
  (AltaVista's `IMAGE_DIGEST.md` convention), with no network at run time and inputs
  fetched by digest only.

### 7.2 Server inventory (modular: one MCP server per capability, all behind `edm-gateway`)

| Server | Backing tool | Tools exposed (examples) | Build / adopt |
|---|---|---|---|
| `edm-graph` | Flexo MMS (SysML v2 API + SPARQL) | `query`, `trace(uid)`, `impact(digest)`, `closure(baseline)`, `plan(interface)` | Build on flexo-mms-sysmlv2-mcp (read-only mode) + our linker |
| `edm-propose` | git forge | `propose_change(files, rationale)` → PR, `propose_requirement`, `propose_waiver` | Build (analogue of `av-gateway` `propose_command`) |
| `cad-freecad` (module #1, *added 2026-09-28*) | FreeCAD 1.1.4 headless, own thin hermetic module | `build_model`, `mass_properties`, `extract_interfaces`, `fem_prepare`, `diff_models`, `export` (full list: `docs/edm/tool-modules.md` §4.4) | Build; ideas reused from neka-nat and spkane (MIT) |
| `cad-code` | build123d/CadQuery on OCCT | `build(model, params)` → STEP + mass props, `query_features`, `check_interface(uid)` | Build (community servers as reference) |
| `cad-vendor` | Onshape FeatureScript MCP, Fusion MCP | vendor-defined | Adopt where licensed |
| `mesh` | gmsh | `mesh(step_digest, size_field)` → mesh + quality | Build |
| `fea-structural` | CalculiX (and code_aster later) | `static`, `modal`, `buckling` → results + extracted scalars | Build (hardened from mcp-calculix idea) |
| `thermal` | Elmer FEM (steady and transient conduction and radiation); lumped-node solver for early phase | `steady`, `transient`, `orbit_thermal(drm_hash)` | Build; `orbit_thermal` consumes AltaVista trajectories and eclipse events for flux boundary conditions |
| `cfd` | OpenFOAM | `case(template, params)` | Build later |
| `ansys` | PyAnsys MCP (Mechanical, Fluent) | vendor-defined, wrapped for attestation | Adopt where licensed |
| `mission` | AltaVista gateway | `run_drm`, `query run products` | Exists (AltaVista) |
| `tracker` | spoore harness | `run_golden`, `scorecard` | Exists (spoore P2) |
| `mfg` | QIF/AP242 toolkit | `inspection_plan(pmi)`, `ingest_qif`, `open_ncr` (propose) | Build |

**Wrapping contract.** Every server is a thin adapter over a shared `edm-jobkit`
library. It does the following:

1. validates the job spec against the schema;
2. resolves inputs by digest, verifying on read;
3. runs the pinned container;
4. hashes the outputs and stores them in the store;
5. extracts scalars with units;
6. writes the attestation and appends to the evidence log;
7. returns `Job`.

Adding a new tool means writing one adapter and passing a **golden analysis**. This is the
spoore "onboarded when it passes the golden scorecard" rule applied to CAE: canonical
benchmark problems with known answers, such as NAFEMS-style benchmarks and analytic
conduction cases, with tolerances justified against measurement.

### 7.3 Solver credibility

Open solvers driven by agents are only as good as their V&V pedigree. Each analysis
`Result` therefore carries a MoSSEC-style **credibility record**:

- which golden benchmarks the tool/version passed;
- the mesh convergence evidence (an agent-run refinement study counts, since it is itself
  jobs);
- the model-form assumptions.

This is spoore's "assumption ledger" idea applied to CAE. A `VerificationResult` by
Analysis on a DAL-A/B requirement must cite a credibility record at or above the level
policy sets.

---

## 8. Large artifacts

- **Content addressing everywhere.** Git holds references (`AssetRef{uri, sha256,
  size, media_type, label, provenance}`), never payloads.
- **The store is pluggable behind one interface:** OCI registry via ORAS (default,
  mirrors cleanly air-gapped), MinIO/S3 via `av-store`, and lakeFS for mutable result
  lakes. Every read verifies size and sha256, as `av-store` does.
- **Chunking and dedup.** Large meshes and results (10–500 GB) are stored as chunked
  layers. Zarr or HDF5 chunk manifests let extractors read only the fields they need.
  Q-E5 covers whether to add a chunk-level dedup store.
- **Retention tiers:**
  - Anything referenced by a baseline or a closed `VerificationResult` is pinned forever.
  - Intermediate agent exploration artifacts are TTL-garbage-collected unless promoted.
  - Garbage collection is driven by graph reachability, not age alone.
- **Labels** (CUI, ITAR, export control) come from the AltaVista `Label` and are checked
  **before** fetch. The gateway filters graph queries by the caller's clearance.
- **Visualization.** Derived lightweight products are themselves artifacts linked by
  `producedBy`: glTF for CAD, and decimated VTK/VTU or Zarr for results. They can be
  viewed in AltaVista's Three.js viewer (question for the viewer track).

---

## 9. Repository layout (proposed new repo `edm`; this plan lives in AltaVista until it exists)

```
edm/
├── proto/edm/v1/              # edm.v1 records (imports altavista.v1 core)
├── crates/                    # Rust, matching the AltaVista/spoore workspace idiom
│   ├── edm-model              # generated types, canonical hashing, YAML mirror
│   ├── edm-link               # parser adapters (sysml, sdoc, yaml) → typed graph; UID/link refusals
│   ├── edm-closure            # closure, staleness, impact, baselines
│   ├── edm-plan               # interface reconciliation + extractor protocol
│   ├── edm-policy             # Rego (regorus) gates
│   ├── edm-attest             # in-toto predicates, cosign, evidence chain
│   ├── edm-store              # store trait: ORAS, av-store, lakeFS
│   └── edm-gateway            # MCP + gRPC, OIDC, labels, propose-only
├── python/edm-jobkit/         # MCP tool-server SDK: job spec, digest I/O, attestation, extractors
├── tools/                     # one dir per MCP tool server + Dockerfile + IMAGE_DIGEST.md + goldens
│   ├── cad-freecad/  cad-code/  mesh/  fea-structural/  thermal/  mfg/   (contract: docs/edm/tool-modules.md)
├── sysml/lib/                 # program-neutral SysML v2 libraries (interface kinds, TAID, units)
├── examples/cubesat-panel/    # the worked example (§11 P2)
├── deploy/                    # secdeploy fragment, Flexo + registry + lakeFS, air-gap bundle
└── docs/ adr/ open-questions.md compliance/
```

**Language split.**
- The core (linker, closure, policy, attestation, gateway) is Rust. This is for
  determinism and reuse of AltaVista crates.
- The tool servers are Python, because the CAD/CAE ecosystem is Python (build123d,
  gmsh, PyAnsys, meshio).
- The boundary between them is protobuf/MCP, which is spoore's rule.

---

## 10. Relationship to spoore and AltaVista

- **AltaVista:**
  - AltaVista is the **validation engine** for mission-level needs. DRM `Objective`s
    map to `ValidationCase` constraints, and `RunProducts` plus `ScoreResult` are
    evidence consumed by hash.
  - `SystemDefinition`/`Port`/`Parameter` are generated from, or checked against, the
    SysML model. That makes `Port.schema` and `interface_class` the data-interface
    control parameters.
  - AltaVista's control matrices become one *view* of the EDM graph: NIST controls
    modelled as requirements, with the implementation `file:function` and evidence
    commands as verification.
- **spoore:**
  - spoore golden scorecards (RMSE, NEES/NIS, latency p99) are verification evidence
    for tracker performance requirements.
  - The spoore P0–P4 exit criteria are themselves requirements that can be tracked in the
    EDM, a cheap first dogfood.
- **Shared core.** `Provenance`, `AssetRef`, `Label`, `Unit`, time, canonical hashing and
  the evidence chain should eventually move to a shared `common.v1` package. Until then
  `edm.v1` imports `altavista.v1` (Q-E1).

---

## 11. Phases and exit criteria

**E0 — Foundations (wk 1–3).**
- Work: repo, CI, ADR-E000..E005 (scope, git-as-truth, graph substrate, identity and
  hashing, attestation predicates, tool-plane authority), and the `edm.v1` proto with
  `buf breaking` on.
- Exit: `edm-link` parses a toy `.sysml` + `.sdoc` + YAML set into one graph and refuses a
  dangling link and a duplicate UID with typed errors; the canonical hash matches between
  Rust and Python.

**E1 — Trace skeleton (wk 3–7).**
- Work: requirements (SysML + StrictDoc), trace links, closure query, orphan report and
  VCRM generation; baseline tags and manifests; load into Flexo MMS (pinned) and query
  via the SysML v2 API and SPARQL; ReqIF import/export round-trip.
- Exit: the spoore plan's P0–P4 exit criteria are modelled as requirements and closed
  from spoore CI evidence (dogfood); ReqIF round-trips through StrictDoc without loss on
  a golden file.

*Amended 2026-09-28: the phases now start with a research phase (R), and E0–E5 absorb the
compliance capabilities. Capability IDs refer to `docs/edm/compliance-crosswalk.md`.*

**R — Research and planning (current).**
- Work:
  - spikes R1–R8 in `docs/edm/tool-modules.md` §7 (FreeCAD headless coverage, determinism,
    models-as-code usability, the PMI path, FCStd normalization, interface-extraction
    robustness, the accreditation record shape, and a private Sigstore under FIPS);
  - verifying the [U]/[P] items in the crosswalk (§7);
  - drafting ADR-E000..E005 as Proposed.
- Exit: every spike has a written finding, and the ADRs are ready for acceptance.

**E0 additions.** The `ComplianceMatrix` / tailoring records (C00), the `Label` on every node
(C14), the `ToolVersion` / `Accreditation` records (C07), and the toolchain supply-chain
baseline (C15: SBOM, SLSA, STIG'd images).

**E2 — Artifact thread + first tools (wk 6–12).**
- Work:
  - `edm-store` (ORAS + av-store);
  - `edm-attest` with the predicates in §5.3 plus `edm.dev/job/v1`;
  - the runner and module contract (`docs/edm/tool-modules.md` §2–3);
  - **`cad-freecad` (module #1)**, then `fem-calculix` and `thermal-elmer`, each with its
    accreditation suite;
  - the `AnalysisRecord` (C06);
  - staleness and impact.
- Exit: **the worked example**, a small-sat radiator/equipment panel:
  - `REQ-THM-012` "board interface ≤ 60 °C in worst-hot case" and `REQ-STR-004` "first
    mode ≥ 100 Hz";
  - FreeCAD panel (models as code) → `fem_prepare` → Elmer (thermal) and CalculiX (modal),
    each result carrying an `AnalysisRecord` whose credibility is at or above the tailored
    threshold;
  - both requirements closed with attestations;
  - a parameter change in the CAD shows the correct `edm impact` and re-opens both.

**E3 — Interfaces as code (wk 10–16).**
- Work: interface declarations, owners → CODEOWNERS, extractors (STEP features, code-CAD
  features, thermal results, AltaVista port traffic), `edm plan`, Rego gates, ICD
  generation, and AsyncAPI/proto generation for data interfaces.
- Exit: a seeded bolt-pattern mismatch and a seeded message-schema mismatch are both
  caught by `edm plan` in CI; a CDR-gate query runs against a baseline.

**E4 — Agents (wk 12–18).**
- Work: `edm-gateway` (MCP, OIDC, labels, deny-by-default) and `edm-propose`; agent
  workflows:
  - "decompose this need into requirements" (propose);
  - "size this panel to close REQ-STR-004" (run jobs, propose a CAD PR with evidence);
  - "explain what this change invalidates".

  An NL→artifact golden set runs in CI (AltaVista question 70 pattern).
- Exit: an agent closes the E2 example's modal requirement after a seeded failure, with
  a human merging the PR. Every step is reconstructible from the evidence log.

**E5 — Manufacturing + validation (wk 16–24).**
- Work: MBOM, CycloneDX HBOM export, QIF inspection plans from PMI, `SerializedUnit`
  genealogy, NCR workflow with re-analysis on as-built values; an AltaVista DRM as a
  validation case; air-gap bundle.
- Exit: a simulated as-built unit with one out-of-tolerance QIF feature → NCR → automatic
  re-analysis → use-as-is disposition with evidence → PCA query passes. Clean install with
  zero egress.

This assumes 2–3 engineers. E2 tool servers parallelize with E1. A CAD/CAE-literate
engineer is on the critical path from E2.

---

## 12. Open questions (numbered, answer inline as AltaVista does)

- **Q-E1.** Should `edm.v1` import `altavista.v1` core types directly, or should we first
  extract a shared `common.v1` package used by spoore, AltaVista and EDM?
  - Default: import now, extract when a third consumer needs it.
- **Q-E2.** Is git or Flexo the authority? Default: git (ADR-E001); falsifier in §3.
  - Does the team expect graphical SysML v2 authoring (SysON, Cameo), or text-first
    (Syside, VS Code)?
- **Q-E3.** Which CAD system(s) are in use today? This decides adopting vendor MCPs versus
  building on code-CAD.
  - Default: code-CAD (build123d) for agent-driven work, with STEP AP242 as the neutral
    exchange for everything else.
  - **Answer (2026-09-28):** every tool is a module, and **FreeCAD is first**. It is exposed
    over MCP by our own thin, hermetic module. Neither the neka-nat nor the spkane server is
    forked; ideas and code are reused from both under their MIT licences. See
    `docs/edm/tool-modules.md` §4. build123d becomes module #5.
- **Q-E4.** Which analysis tools are licensed? This decides whether PyAnsys MCP is
  adopted or open solvers only.
  - Default: open solvers first (CalculiX, Elmer), Ansys wrapped when available.
- **Q-E5.** Artifact store default: OCI/ORAS or MinIO (`av-store`)?
  - Default: OCI for immutable artifacts (signing and referrers built in), lakeFS for
    result lakes, `av-store` as a backend option.
  - Is chunk-level dedup needed at expected volumes? That needs a size estimate from the
    team.
- **Q-E6.** Which assurance regime governs? DO-178C/DO-254/ARP4754B, NASA NPR 7123.1,
  DoDI 5000.97, or commercial.
  - This sets the default closure rules and DAL handling.
  - **Answer (2026-09-28): DoD and NASA.** The merged obligations are in
    `docs/edm/compliance-crosswalk.md`. DO-178C/ARP4754B stay relevant only for airborne
    items, through MIL-HDBK-516C.
- **Q-E7.** Is there an existing requirements tool (DOORS, Jama, Polarion) that must remain
  master for some programs?
  - If so, the EDM syncs via ReqIF/OSLC and treats it as an external authority with
    aliases.
  - **Answer (2026-09-28): no, the program is greenfield.** The EDM is the ASOT (DoDI
    5000.97). ReqIF/OSLC are kept only for customer and supplier exchange.
- **Q-E8.** Should Syside Automator (paid) be licensed for a fast Python SysML v2 API, or
  should we stay on the EPL pilot implementation plus our own linker?
- **Q-E9.** Several survey items could not be verified directly (openmbee.org was
  unreachable from the research environment). These are the AsyncAPI version, the ReqIF
  1.2 version, the DVC ownership change and the ARP4754B date. Confirm them before any
  ADR relies on them.
- **Q-E10.** Should the program-level project name, repo and org (the §9 layout) be
  created now, or should this stay a plan in AltaVista until E0 is ratified?
- **Q-E11.** Where will the enclave be hosted?
  - Options: GovCloud-class hosting with GitHub Enterprise Server or GitLab, an OCI registry,
    a private Sigstore, and enclave LLM inference.
  - Program technical data (CAD, analyses, ICDs) is ITAR/EAR or CUI and **must not** live in
    public GitHub. This planning repo stays free of it. See crosswalk §6.
- **Q-E12.** What is the program model allow-list?
  - The DoD research reports a FASCSA supply-chain-risk designation of Anthropic for DoW
    contract work (effective 2026-03-03, upheld 2026-09-25).
  - **Contracts and legal must confirm what applies per program**, including to the tools used
    to write the EDM.
  - The design is provider-agnostic regardless (`tool-modules.md` §6).
- **Q-E13.** How is AP242 PMI / MBD produced, given that FreeCAD cannot yet do it?
  - Default: carry PMI as EDM data and render it to 2D drawings, pending spike R4.
- **Q-E14.** Which NASA payload risk class and which DoD acquisition pathway does the first
  program fall under? These key the C00 tailoring records.
- **Q-E15.** Who are the named human approvers, and in which roles?
  - Roles: CCB chair, ICWG leads, MRB, Technical Authority, M&S accreditation authority.
  - The CM layer needs them from E1 onward.

---

## 13. Risks and mitigations

| Risk | Mitigation |
|---|---|
| SysML v2 tools diverge and the 2.0→2.1 churn | Pilot implementation pinned as the CI reference parser; our graph keyed by UID, not tool-specific IDs; conformance goldens |
| Flexo MMS is pre-1.0 | Graph is *derived*; Flexo is replaceable by the pilot API server or plain SPARQL store without touching authored truth; pin + contribute upstream |
| Agent-driven analysis produces plausible wrong answers | Golden benchmarks per tool version; credibility records; mesh-convergence required by policy; human merge; DAL-scaled independence rules |
| Evidence staleness ignored under schedule pressure | Staleness is computed, not declared; waivers are records with owner and expiry and appear in every closure report |
| Artifact volume and cost | Reachability GC, TTL on unpromoted exploration, chunked formats, lakeFS for lakes |
| Certification authorities do not accept the new evidence form | Always generate traditional VCRM/trace matrices and ICDs from the graph; attestations are *additional* integrity, not a replacement |
| Engineers reject text-first authoring | SysON/Syside editors over the same text; StrictDoc for prose; ReqIF bridge to incumbent tools |
| Export control / CUI leakage through agents | Labels checked before fetch; gateway filters by clearance; local LLMs by default (secrouter/secllm); no network in tool containers |

---

## 14. Immediate next actions

1. Answer the remaining questions: Q-E1, E2, E4, E5 and E8–E15. Q-E11 (hosting), Q-E12
   (model allow-list) and Q-E14 (risk class and pathway) block the most work.
1a. Run research spikes R1–R8 (`docs/edm/tool-modules.md` §7), starting with R1, R2 and R4.
2. Draft ADR-E000 (scope and lineage) and ADR-E001 (git as the authored truth; Flexo as the
   derived query server).
3. Stand up pinned Flexo MMS and the SysML v2 pilot parser in a dev compose. Load the
   spoore plan's exit criteria as the first requirements (E1 dogfood).
4. Author the §4.5 proto for review. It is not committed to `proto/` until the ADRs are
   accepted, per AltaVista's "Proposed until the user accepts" rule.

---

## References

- OpenMBEE Flexo MMS: https://github.com/Open-MBEE/flexo-mms-layer1-service ·
  SysML v2 adapter https://github.com/Open-MBEE/flexo-mms-sysmlv2 ·
  MCP https://github.com/Open-MBEE/flexo-mms-sysmlv2-mcp
- OMG SysML v2 / KerML / API adoption (2025): https://www.omg.org/news/releases/pr2025/07-21-25.htm ·
  pilot https://github.com/Systems-Modeling/SysML-v2-Pilot-Implementation ·
  API services https://github.com/Systems-Modeling/SysML-v2-API-Services
- SysON https://mbse-syson.org/ · Syside https://docs.sensmetry.com/ · StrictDoc https://strictdoc.readthedocs.io
- OSLC Core 3.0 https://docs.oasis-open-projects.org/oslc-op/core/v3.0/oslc-core.html
- STEP AP242 https://www.iso.org/standard/84300.html · AP243 MoSSEC https://www.iso.org/standard/72491.html
- CycloneDX HBOM https://cyclonedx.org/capabilities/hbom/ · ORAS https://oras.land/ ·
  Sigstore https://docs.sigstore.dev/cosign/signing/other_types/ · SLSA/in-toto https://slsa.dev/blog/2023/05/in-toto-and-slsa
- PyAnsys MCP https://common-mcp.docs.pyansys.com/ · Onshape FeatureScript MCP
  https://www.ptc.com/en/news/2026/onshape-launches-featurescript-mcp-server · awesome-ai-cae
  https://github.com/kimimgo/awesome-ai-cae
- DoDI 5000.97 https://www.esd.whs.mil/Portals/54/Documents/DD/issuances/dodi/500097p.PDF ·
  NASA SE Handbook Rev 2 https://essp.larc.nasa.gov/EVI-6/pdf_files/NASA_SystemsEngineeringHandbookRev2.pdf
- AltaVista: `docs/philosophy.md`, ADR-001 (CDM v1), ADR-004 (security boundary and evidence;
  AI plane), `docs/aiplane-plan.md`, `crates/av-store`, `crates/av-gateway/src/mcp.rs`,
  `docs/compliance/`
- spoore: `spoore-implementation-plan.md` (§3.3 assumption ledger, §7 harness/golden onboarding)
