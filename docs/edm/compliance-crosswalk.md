# EDM compliance crosswalk: DoD + NASA → capabilities

**Status: planning draft (2026-09-28).**

**Sources.**
- The detailed notes are `research/dod-requirements.md` and `research/nasa-requirements.md`.
- Items those notes mark [U] or [P] are carried here unchanged. **No ADR may rely on them until
  they are verified** (§7).

## 1. What "meeting DoD and NASA certs" means for the EDM

Neither agency certifies a data model or a tool. What exists:

- **Program-level certifications and decisions**, supported by evidence:
  - DoD: airworthiness (MIL-HDBK-516C), cyber ATO, safety and interoperability.
  - NASA: technical reviews, KDPs, and certification of flight readiness at FRR.
- **Company-level certifications:** AS9100 (becoming IA9100), NADCAP, CMMC, and DFARS purchasing
  and counterfeit-avoidance systems.
- **Tool and model credibility obligations** that the program has to discharge:
  - NASA: NPR 7150.2D SWE-136 and SWE-070, and NASA-STD-7009B.
  - DoD: DoDI 5000.61, DoDM 5000.102 and MIL-STD-3022.
- **Authorization of the toolchain as an information system:** RMF/CSRMC, NIST 800-171,
  NPR 2810.1, FedRAMP, and ITAR/EAR controls.

**The EDM's job:**
- produce the evidence for every tailored requirement **on demand, at low cost, from one
  source**;
- be an **authorizable, accreditable system** in its own right.

Every capability below exists to serve one of those two goals.

**Design rule (C00): tailoring is data.** Each program's compliance matrices are EDM records,
keyed by the program's classification attributes. Every gate, query and document template takes
its obligations from those records. None is hard-coded.

- **Compliance matrices held as records:**
  - NPR 7123.1D Appendix H;
  - the NPR 7150.2D requirements mapping matrix;
  - 7009B tailoring;
  - the DRD/CDRL list;
  - the SEP and SEMP tailoring;
  - the CM plan.
- **Classification attributes that key them:**
  - NASA payload risk class (NPR 8705.4);
  - software class per CSCI;
  - DoD ACAT and acquisition pathway;
  - DAL and criticality;
  - CUI and export category.

## 2. Capability catalogue

Legend for the phase column: E0–E5 refer to the phases in `../edm-plan.md` §11, as revised in
§11 of that plan.

| ID | Capability | DoD sources | NASA sources | Main EDM objects / queries | Phase |
|---|---|---|---|---|---|
| C00 | Tailoring as data | SEP, CDRL list, EIA-649-1A CM plan | NPR 7123.1D App H, NPR 7150.2D RMM, 7009B tailoring, DRD list, NPR 8705.4 class | `ComplianceMatrix`, `Obligation`, `TailoringDecision` (approver = TA / PM) | E0 |
| C01 | Requirements with verification method and bidirectional trace | MIL-STD-961E §4, DI-IPSC-81434 | NPR 7123.1D #2, #11; SWE-052; SP-6105 | `Requirement` (TAID, level, phase, rationale, TBD/TBR), `TraceLink`; orphan query | E1 |
| C02 | Baselines and technical reviews as queries | DoDI 5000.88, 24748-8, SE Guidebook (SRR/SFR/PDR/CDR/SVR-FCA/PRR/PCA) | NPR 7120.5F / NID 7120.148, NPR 7123.1D App G (MCR…FRR) | `Baseline` (functional/allocated/product), `Review` with entrance/success criteria as Rego; review package frozen at a signed tag | E1 |
| C03 | Configuration management over git | EIA-649C + **649-1A**, MIL-HDBK-61B, ECP DI-SESS-80639, audit plan DI-SESS-81646 | EIA-649C + **649-2**, NPR 7123.1D #14 | `ConfigurationItem`, `ChangeRequest` (ECR/ECP, Class I/II), `Deviation`, `Waiver`, `Effectivity`; status-accounting query. **A merge implements an approved change; it is not the approval** | E1 |
| C04 | Interface management as code | DI-IPSC-81434/81436 (IRS/IDD) | NPR 7123.1D #12 (IRD/ICD/IDD, ICWG) | `Interface` with per-end owners and signatures; `edm plan` drift; generated ICD/IRS/IDD | E3 |
| C05 | VCRM / verification closure | MIL-STD-961E §4, DI-NDTI-80603 (procedure), DI-NDTI-80809 (report), DoDI 5000.89 IDSK | SP-6105 VCRM, NPR 7123.1D #7, #8 | `VerificationCase`, `VerificationResult`, `TestProcedure`, `TestReport`; closure and staleness queries; IDSK query | E1–E2 |
| C06 | **Analysis credibility record** (unified 7009B + 3022) | DoDI 5000.61 (2024), DoDM 5000.102, MIL-STD-3022 | NASA-STD-7009B, HDBK-7009B (incl. AI guidance), SWE-070 | `AnalysisRecord` (§3); `CredibilityAssessment`; the four MIL-STD-3022 products generated | E2 |
| C07 | **Tool accreditation registry** | 5000.61 VV&A for intended use; MIL-HDBK-516C §15 (DO-330 for airborne tools) | **SWE-136**, SWE-070 | `ToolVersion`, `Accreditation` (intended use, golden suite results, approver); dependents invalidated on version change (§4) | E0–E2 |
| C08 | Safety and hazards linked to requirements | MIL-STD-882E Chg 1; DI-SAFT-80101/80102 | NPR 8715.3, NASA-STD-8739.8B (software hazards), 8719.x | `Hazard` → mitigating `Requirement` → verification; risk acceptance authority | E1 |
| C09 | FMECA / R&M | DI-SESS-81495 | NASA-STD-8729.1 (FMEA/CIL) | `FailureMode` linked to parts, functions and detection; critical items list (CIL) query | E3 |
| C10 | Structural, thermal and M&P discipline data | MIL-STD-1530D, SMC-S-016, 810H, **461H** | NASA-STD-5001B, 5002, 5019A, 5020, **6016C (MUA)**, GEVS, 7002, 7012 | Factor-of-safety and margin-of-safety fields on analysis results; `LoadSet` versions; fracture class; `Material` with allowables pedigree; `MUA`; environmental test matrix | E2–E5 |
| C11 | Model-based definition / TDP | **MIL-STD-31000B**, DI-SESS-81000, AS9102C | NASA DRDs (drawings/models) | TDP index; AP242 **with PMI**; key characteristics (AS9103); rights and distribution markings. **FreeCAD gap: §5** | E5 |
| C12 | As-built, quality and supply chain | AS9100D/IA9100, AS9102C, AS9145, **DFARS 252.246-7007/-7008**, AS5553 | NPR 8735.2C (GMIP), NPD 8730.5, 8739.10, NPR 8735.1D (GIDEP), J-STD-001 space addendum | `SerializedUnit` with genealogy, `Traveler`, `FAI`, `NCR`/MRB (use-as-is → waiver), `GMIPHold`, part pedigree; GIDEP where-used query | E5 |
| C13 | Software assurance of tracked flight software | JSSSEH; MIL-STD-882E software criticality | NPR 7150.2D (class per CSCI), 8739.8B, SWE-141 IV&V | Software class per CSCI; RMM compliance; coverage/static-analysis evidence; IV&V export | E3+ |
| C14 | Data markings, export control, rights | DoDI 5230.24, 5200.48, DFARS 252.227-7013/-7014/-7017, ITAR 22 CFR 120.54(a)(5) | NPR 2190.1C, NID 2810.135 | `Label` on **every** node and artifact (CUI category, ITAR/EAR, rights legend, distribution statement); propagates to derived artifacts; enforced by store, graph and agent context builder | E0 |
| C15 | Toolchain authorization | DoDI 8510.01 / **CSRMC**, 800-53 r5.2 (SA-24), 800-171 r2 (target r3), DFARS 7012, CMMC (Phase 2 suspended), Iron Bank, STIGs, **SWFT**, SSDF | NPR 2810.1F, FedRAMP, 800-171 | SBOM + SLSA/in-toto per EDM release; STIG'd images; audit logs; continuous-monitoring feed; enclave deployment (§6) | E0 onward |
| C16 | AI provenance and model supply chain | DoW AI Strategy 2026, OMB M-25-21/-22, **M-26-04**, NIST AI 600-1, **FASCSA exclusions** | NASA GenAI guidance, HDBK-7009B AI guidance, SWE-136 | `AgentAction` attestation (provider, model and version, context digest, tools called, outputs, human approver); **model allow-list per program**; provider-agnostic MCP layer | E4 |
| C17 | Document generation (DRDs/CDRLs) | DIDs above; SEMP DI-SESS-81785A | DRDs per contract; NASA-HDBK-1009A work products | Reproducible renderers from the graph at a baseline; DRD/CDRL submittal status | E1 onward |
| C18 | Digital engineering and ASOT | **DoDI 5000.97**, DE Strategy, OUSD(R&E) SysML v2 info sheet (Feb 2026) | NASA-HDBK-1009A | git (authored) + Flexo (derived) as the ASOT; curation status and owner per model; models delivered as data; SysML v2 ↔ HDBK-1009A mapping | E0–E1 |
| C19 | WBS | MIL-STD-881F | NPR 7120.5 WBS | WBS element on every CI and task | E1 |

## 3. The unified analysis record (C06)

This record is the most important schema for the tool plane. Every FEA, thermal or CFD result
that could close a verification carries one.

It merges three sources:
- the NASA-STD-7009B credibility factors;
- the DoDI 5000.61 / MIL-STD-3022 VV&A products;
- the AP243 MoSSEC concepts from the original plan.

| Field group | Contents | Satisfies |
|---|---|---|
| Intended use | Use statement; decision supported; requirement UIDs; M&S risk (consequence × influence); tailoring reference | 7009B use statement / risk; 3022 Accreditation Plan |
| Model identity | Tool `ToolVersion` digest (C07); container digest; geometry digest; mesh digest and mesh-quality report; element types; BCs and loads spec digest; `LoadSet` version; material cards with `Material` / allowables pedigree | 7009B input pedigree, M&S management; 5000.61 CM |
| Assumptions and limits | Abstractions; linearity and other assumptions; **limits of operation** (ranges of parameters the model was validated over); automatic flag if the run is outside those limits | 7009B; spoore assumption-ledger pattern |
| Code verification | Golden benchmark suite results for this `ToolVersion` (from C07) | 7009B Verification; 3022 V&V Report |
| Solution verification | Mesh-convergence study (a set of `Job`s); numerical error estimate | 7009B Verification |
| Validation | Referent data digests (test correlation, e.g. modal survey or thermal balance); comparison metric and result | 7009B Validation; 3022 V&V Report |
| Uncertainty | Input and model-form UQ; sensitivity and robustness study results | 7009B Results Uncertainty / Robustness; 5000.61 UQ emphasis |
| Results | Extracted quantities with units and uncertainty bounds; factors of safety used (5001B); **margin of safety** | 5001B; 7009B reporting |
| Credibility assessment | Score 0–4 per 7009B factor, with rationale; required threshold from C00 tailoring; pass/fail | 7009B; 3022 Accreditation Report |
| People and agents | Analyst; independent reviewer; **agent identity (provider, model, version) and human approver** if agent-run | 7009B People Qualifications; HDBK-7009B AI guidance; C16 |
| Use history | Prior accepted uses of this model/tool for similar intended use | 7009B Use History |

**Closure rule.** A `VerificationResult` by method Analysis closes a requirement only if all of
the following hold:
- its `AnalysisRecord` credibility meets the tailored threshold for that requirement's
  criticality;
- its `ToolVersion` is accredited (C07) for that intended use;
- none of its input digests is stale.

## 4. Tool accreditation lifecycle (C07)

This applies uniformly to every tool module (`tool-modules.md`), to the EDM itself, and to agent
models.

1. **Register the `ToolVersion`:**
   - image digest;
   - upstream versions (for example FreeCAD 1.1.4 and OCCT 7.x);
   - lockfile hash;
   - SBOM digest;
   - licence inventory.
2. **Define the intended use.** Examples: "parametric part geometry and mass properties for
   structural parts"; "linear static stress, isotropic metals".
3. **Run the golden suite for that intended use.** Examples:
   - analytic solutions;
   - NAFEMS-style benchmarks;
   - reference parts with known mass properties.

   Record the residuals against tolerances that are justified by measurement. This is
   AltaVista's goldens rule: a miss records the residual, never a loosened tolerance.
4. **A named human accredits it.** The `Accreditation` record is signed and carries its scope and
   any limitations. Under NPR 7150.2 this is SWE-136; under DoD it is 5000.61 VV&A for that
   intended use.
5. **Handle version changes.** A new FreeCAD/OCCT/solver version, image rebuild or model version
   is a new `ToolVersion`:
   - Results produced by the old version stay valid for the baselines they were produced under.
   - New closures need an accredited version.
   - `edm impact` lists what a tool upgrade would re-open.
   - FreeCAD's move to three CalVer releases a year makes this a routine event, so it must be
     cheap and fully automated up to the human signature.
6. **LLM models are `ToolVersion`s too.**
   - Their "accreditation" is scoped to *proposing* changes, never to approving them.
   - The NL→artifact golden set (AltaVista question 70 pattern) is their suite.
   - They are checked against the program's model allow-list (C16).

## 5. Known gaps and how the plan handles them

| Gap | Impact | Handling |
|---|---|---|
| **FreeCAD cannot emit AP242 PMI / semantic GD&T** (proposal only: issues #29772/#29797) | MIL-STD-31000B MBD TDPs, AS9102C model-based FAI and QIF-from-PMI (C11/C12) cannot come from FreeCAD alone | (a) Carry PMI as EDM data (key characteristics, tolerances and datums on named features) and render it to 2D TechDraw drawings (a drawing-based TDP is still allowed under 31000B). (b) Evaluate a PMI authoring path: an OCCT XDE-based writer in our own module, a commercial MBD tool as a separate module, or contributing to FreeCAD's PMI effort. Research spike R4 in `tool-modules.md` |
| No NASA or DoD rule is specific to AI-authored engineering artifacts | Reviewers may apply ad-hoc expectations | C16 plus the "agent proposes, human approves" CM rule (C03), plus the HDBK-7009B AI guidance. Document this in an ADR and socialize it early with the program TA / DCMA |
| CMMC Phase 2 suspended; the Task Force report is pending | Assessment scope and timing are uncertain | Build to 800-171 r2 controls now, with r3 ORDs as the target. Re-check when the Task Force report lands |
| NPR 7120.5F expired 2026-08-03; the interim NID's status is unconfirmed | Review criteria source | C00 makes the governing document a record, so switching sources is data, not code |
| SysML v2 has no NASA HDBK-1009A profile | Reviewers expect familiar work products | Maintain a SysML v2 ↔ HDBK-1009A mapping; generate the familiar products (C17) |

## 6. Hosting and supply-chain implications

These are decisions for the program, not for this plan.

- **ITAR/CUI data cannot sit in public GitHub or a public transparency log.**
  - Use GitHub Enterprise Server (or GitLab) in a GovCloud-class, US-person-operated enclave.
  - Run a **private Sigstore stack** (Fulcio, Rekor, TUF) with FIPS 140-validated crypto.
  - Run the OCI registry and object store inside the enclave.
- **Build with Iron Bank / STIG'd base images** where possible. Each EDM release ships an SBOM,
  SLSA provenance and a SWFT-ready package.
- **LLM inference for ITAR data needs an enclave endpoint.** The ITAR encryption carve-out does
  not help, because the model must see plaintext. AltaVista's secrouter/secllm "local by default"
  posture fits this.
- **Model supply chain.** The DoD research reports that the Department of War designated
  Anthropic a supply-chain risk under FASCSA (effective 2026-03-03, upheld by the DC Circuit
  2026-09-25), barring Claude in DoW contract work. It does **not** bar non-DoW use.
  - **Contracts and legal must confirm per program** what applies, including to the development
    tooling used to write the EDM.
  - The EDM design is unaffected in shape: the MCP tool plane never assumes a provider, and the
    provider/model is a checked configuration item (C16).
- **This planning repository is on GitHub.**
  - Keep it free of program technical data.
  - Program models, CAD and results belong only in the enclave instance.

## 7. Verify before any ADR relies on this

**From the NASA note:**
- the NID 7120.148 extension or an NPR 7120.5G;
- the exact 7009B / HDBK-7009B text, including the factor list;
- whether NPR 8705.4B is in effect;
- revisions of NASA-STD-5020, 7002 and 8739.10, and NPD 8730.5.

**From the DoD note:**
- the DoDI 5000.97 §3–4 text;
- revisions of AS6174, AS9103 and AS9145;
- the MIL-STD-1540 cancellation;
- the CMMC Task Force outcome;
- IA9100 publication.

**Both:** have counsel confirm the solver GPL obligations when images are delivered, and the FASCSA
applicability.
