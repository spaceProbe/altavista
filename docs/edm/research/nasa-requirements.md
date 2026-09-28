# NASA requirements → EDM obligations (research note)

Research date: 2026-09-28. This is part of the EDM plan (`docs/edm-plan.md`).

**Method.** The research environment's proxy blocked nodis3.gsfc.nasa.gov, standards.nasa.gov,
swehb.nasa.gov and NTRS. Revisions and dates therefore come from search-engine extracts of those
official pages. Clause-level content comes from domain knowledge.

**Tags.**
- **[V]** confirmed from a search extract of an official NASA page.
- **[K]** domain knowledge, not re-checked.
- **[U]** unverified; must be checked before an ADR relies on it.

The items to verify first are listed in §10.

## 0. Framing: there is no "NASA cert"

NASA does not certify tools or data models. Compliance flows through contracts and directives:

- Each program tailors NPRs and standards into a compliance matrix. Two examples are NPR 7123.1D
  Appendix H and the NPR 7150.2D Appendix C requirements mapping matrix.
- Deviations and waivers go through the Technical Authority (TA).
- Deliverables are defined by contract DRDs (Data Requirement Descriptions).

"Meeting NASA certs" in practice means one thing. For every tailored requirement, the EDM must be
able to produce auditable evidence on demand, showing who approved what, when, and against which
baseline [K]. Flight certification, for example certification of flight readiness at FRR, is a
program decision backed by that evidence.

## 1. Program/project and systems engineering

### Governing program/project directive

- **NPR 7120.5F** took effect 2021-08-03. Its nominal expiry is 2026-08-03.
- **NID 7120.148** (effective 2024-12-09) is an interim directive layered over it [V].
  https://nodis3.gsfc.nasa.gov/OPD_Docs/NID_7120_148_.pdf
- No public NPR 7120.5G was found [U]. Confirm the governing document on NODIS.

### Life cycle, KDPs and reviews

Phases [K]:

| Phase | Name |
|---|---|
| Pre-A | Concept studies |
| A | Concept and technology development |
| B | Preliminary design and technology completion |
| C | Final design and fabrication |
| D | System assembly, integration and test, launch and checkout |
| E | Operations and sustainment |
| F | Closeout |

- KDPs A–F sit between phases.
- KDP-C confirms the Agency Baseline Commitment.
- A JCL is required for projects with life-cycle cost above $1B [V].

Reviews [K]: MCR → SRR → MDR/SDR → PDR → CDR → PRR → SIR → TRR (per test) → SAR/PSR → ORR → FRR/MRR
→ PLAR → CERR → DR/DRR.

Entrance and success criteria are tabulated in **NPR 7123.1D Appendix G**:
https://nodis3.gsfc.nasa.gov/displayDir.cfm?Internal_ID=N_PR_7123_001D_&page_name=AppendixG

- **PDR:** V&V and integration plans are baselined. This requirement is new in 7123.1D [V].
- **CDR:** build-to baseline, and ICDs baselined.
- **TRR:** procedures, facility and test-article configuration are known.
- **SAR:** as-built versus as-designed is verified [K].

### NPR 7123.1D common technical processes

NPR 7123.1D is effective 2023-07-05 and expires 2028-07-05 [V]. It defines 17 common technical
processes:

| Group | Processes |
|---|---|
| System design (1–4) | Stakeholder Expectations Definition, Technical Requirements Definition, Logical Decomposition, Design Solution Definition |
| Product realization (5–9) | Implementation, Integration, Verification, Validation, Transition |
| Technical management (10–17) | Technical Planning, Requirements Management, Interface Management, Technical Risk Management, Configuration Management, Technical Data Management, Technical Assessment, Decision Analysis |

- Bidirectional traceability is explicit in Requirements Management and Logical Decomposition.
- The EDM directly implements #7, #8, #11, #12, #14 and #15 [K].

### NASA SE Handbook SP-2016-6105 Rev 2 [K]

- It uses a VCRM.
- Verification methods are Test, Analysis, Inspection and Demonstration (TAID).
- Verification is "built right to requirements". Validation is "right product for the stakeholder
  expectations and ConOps".
- Each requirement carries a method, level, phase, success criteria and closure artifact.

### NASA-HDBK-1009A, Systems Modeling Handbook for SE

- Approved 2025-03-12 [V].
- It maps SysML v1 work products to NPR 7123.1: MOE/MOP/TPM, ConOps, requirements, and V&V
  planning, results and reports.
- NASA has published no SysML v2 profile [U]. The EDM must maintain its own
  HDBK-1009A ↔ SysML v2 mapping so reviewers see familiar products.
- No NASA-wide NPR mandating MBSE or digital engineering was found as of 2026-09 [U].

## 2. Credibility of models and simulations (critical for agent-driven FEA and thermal analysis)

- **NASA-STD-7009B** was approved 2024-03-05. It has 43 mandatory requirements, structured around
  the M&S life cycle (development versus use) [V].
  https://standards.nasa.gov/standard/NASA/NASA-STD-7009
- **NASA-HDBK-7009B** was approved 2026-02-03 [V]. It adds:
  - a delegated TA approach to tailoring;
  - an updated credibility assessment;
  - **guidance on applying 7009B to AI models**;
  - a new appendix on **use statements**.
  https://standards.nasa.gov/standard/NASA/NASA-HDBK-7009

### Credibility factors

There are eight credibility factors [V for the lineage; exact 7009B wording U]:

1. Verification
2. Validation
3. Input Pedigree
4. Results Uncertainty
5. Results Robustness
6. Use History
7. M&S Management
8. People Qualifications

- Each factor is scored on levels 0–4.
- The rigor required scales with M&S risk, which is the consequence of a wrong decision combined
  with how much the decision depends on the M&S.

### What an analysis record must carry

These fields follow the 7009 structure [K]. Confirm the clause numbers against 7009B.

1. The use statement, the decision it supports, the M&S risk assessment, and any TA tailoring.
2. The model identity under CM:
   - solver and version;
   - mesh and discretization, and element types;
   - boundary conditions and loads;
   - material cards, with pedigree to the M&S database or MUA;
   - input sources and their pedigree.
3. Assumptions, abstractions and **limits of operation**. Any use outside the validated domain is
   flagged.
4. Code verification (solver benchmarks) and solution verification (mesh convergence and error
   estimates).
5. Validation evidence: the referent data (test correlation) and the comparison metric.
6. Uncertainty quantification (input and model-form), sensitivity and robustness.
7. Results with uncertainty bounds, margins of safety, and the per-factor credibility score with its
   rationale.
8. People qualifications: the analyst and the reviewer. For agent-run work, the agent/model identity
   and version and the human approver.
9. Use history, and the reporting statement to decision makers.

Agents that set up or run analyses are themselves M&S or tools in the loop. The HDBK-7009B AI
guidance applies to them.

## 3. Software

### NPR 7150.2D

NPR 7150.2D is current; no public 7150.2E was found [U].
https://nodis3.gsfc.nasa.gov/displayDir.cfm?t=NPR&c=7150&s=2D

Software classes are defined in Appendix D:

| Class | Scope |
|---|---|
| A | Human-rated space |
| B | Non-human space-rated |
| C | Mission support, or a major engineering/research facility |
| D | Basic science and engineering design |
| E | Design concept and R&T |
| F | General-purpose computing |

- Tailoring is by class, using the Appendix C RMM.
- SWE-020 requires every piece of software to be classified.

Key SWEs for the EDM:

- **SWE-052:** bidirectional traceability [V].
- **SWE-079/080:** CM plan and change tracking [V].
- **SWE-070:** models, simulations and tools used for V&V must be verified, validated and
  accredited.
- **SWE-136:** "validate and accredit software tool(s) required to develop or maintain software"
  [V]. https://swehb.nasa.gov/spaces/SWEHBVD/pages/102695495/SWE-136+-+Software+Tool+Accreditation
- **SWE-141:** IV&V for Category 1 projects, and for Category 2 projects with Class A/B payload
  risk. The provider is the Katherine Johnson IV&V Facility [V].

### (a) Flight software tracked in the EDM

- Class per CSCI.
- RMM compliance with tailoring.
- The trace system requirements → software requirements → design → code → tests.
- Coverage: MC/DC for Class A safety-critical software [K].
- Static analysis.
- Hazard links.
- VDDs.

### (b) The EDM and toolchain itself

- The EDM is probably Class C, D or E, depending on use [K/U]. The classification must be recorded
  and justified.
- Where the EDM produces or verifies flight products, SWE-136 and SWE-070 accreditation applies
  **to the EDM, FreeCAD, the solvers, the MCP servers and the agents**. This holds regardless of
  class.
- LLM agents are tools whose outputs must be checked independently. "Agents propose, humans
  approve", backed by deterministic re-verification, is the defensible accreditation argument.

### NASA-STD-8739.8B

- Dated 2022-09-08 [V]. It merges software assurance and software safety, cancels 8719.13, and adds
  IV&V.
- It requires SA independence and hazard analyses with software causes.
- The EDM must give SA personnel read access and audit trails.

## 4. Discipline standards (CAD, structures, thermal, test)

| Standard | Status | EDM data it implies |
|---|---|---|
| NASA-STD-5001B w/Chg 3 (2022-10-24) [V] | Structural design and test factors of safety | FoS per part; test option (prototype/protoflight); MS results tied to load cases |
| NASA-STD-5002 [K] | Load analyses, CLA cycles | Load-set version; CLA cycle ID |
| NASA-STD-5019A [K] | Fracture control | Fracture-critical classification; NDE records; serialization |
| NASA-STD-5020 (B rev, date U) | Threaded fastening | Torque specs linked to joint/preload analyses |
| NASA-STD-6016C w/Chg 1 (2023-11-15) [V] | Materials and processes | Material per part with allowables source; **MUA** objects with approval; outgassing, flammability and SCC ratings |
| NASA-STD-7002 (B) [K] | Payload test requirements | Test matrix |
| GSFC-STD-7000B GEVS (2021-04-29) [V] | Environmental verification (vibe, acoustic, shock, EMC, TVAC and thermal balance) | Test levels; thermal margin conventions; test–analysis correlation |
| NASA-STD-7012 [K] | Leak test | Leak-rate requirements and records |
| J-STD-001 + Space Addendum; NASA-STD-8739.1 and .6 [V] | Workmanship (8739.2/.3 cancelled 2011) | Workmanship callouts; operator certifications |
| NASA-STD-8729.1 [K] | R&M | FMEA/CIL as graph objects |
| NPR 8705.4A (2021-04-29) [V] (NPR 8705.4B existence [U]) | Payload risk class A–D | Top-level class attribute driving tailoring |
| NPR 8715.3, NASA-STD-8719.x [K] | Safety, debris, range | Hazard reports and debris assessment linked to design |

JPL D-17868 "Design Principles" is JPL-internal and not public [U].

## 5. Configuration and data management

- NASA-STD-0005 has been inactive for new design since 2015-03-09 [V]. It is superseded by **SAE
  EIA-649-2** "CM Requirements for NASA Enterprises", which sits on **SAE EIA-649C** [V].
  https://standards.nasa.gov/standard/NASA/SAE-EIA-649-2
- EIA-649 functions: planning, identification, change control, status accounting, and
  verification/audit (FCA/PCA).
- Interface management (NPR 7123.1D #12):
  - **IRD/IRS** holds the requirements.
  - **ICD** is the agreed and controlled design; both sides sign it.
  - **IDD** is a one-sided description.
  - ICWGs disposition changes.
- The EDM must provide:
  - functional, allocated and product baselines;
  - ECR/ECP → CCB disposition;
  - deviations and waivers;
  - effectivity;
  - status-accounting queries (as-designed versus as-built by serial and lot).

## 6. Quality

- **NPR 8735.2C:** hardware QA. GMIPs are workflow holds that need government sign-off [V].
- **NPD 8730.5:** an AS9100-based QMS [K].
- **NASA-STD-8739.10:** EEE parts management [K].
- **NPR 8735.1D:** GIDEP and NASA Advisories, including counterfeit reporting [V].
- **NCR/MRB dispositions** are use-as-is, repair, rework or scrap. A use-as-is or repair
  disposition that affects form, fit or function becomes a waiver or deviation, and needs
  design-authority approval [K].

## 7. IT security, export control and AI

- **NPR 2810.1F** (Security of IT) aligns with NIST SP 800-171 for CUI, and NID 2810.135 covers CUI
  [V]. Adoption of 800-171 Rev 3 versus Rev 2 in NASA contracts is [U].
- **NPR 2190.1C** (Export Control) [V]:
  - Most spacecraft CAD files, FEA decks and ICDs are ITAR/EAR technical data.
  - Git remotes, artifact stores, transparency logs and LLM endpoints must be controlled by US
    persons.
  - **A public Rekor transparency log leaks metadata.** Use a private Sigstore/Rekor instance.
- **FedRAMP** Moderate is the typical floor for hosting CUI. ITAR adds US-person and data-residency
  controls.
- **AI:**
  - NASA's AI Strategy Board was chartered 2025-01-28 [V].
  - OMB M-25-21 (2025-04) requires agency GenAI policies [V].
  - NASA guidance [V]:
    - use only approved GenAI tools;
    - SME review of AI output;
    - no ITAR/EAR data in public AI systems;
    - disclose AI use.
  - No NASA NPR specifically governs AI in engineering design [U]. Treat HDBK-7009B's AI guidance
    plus SWE-136 as the de facto control framework.
  - https://files.gao.gov/reports/GAO-25-107653/index.html

## 8. Requirement source → EDM obligation → phase

| Source | EDM must provide | Phase |
|---|---|---|
| NID 7120.148 / NPR 7120.5F | Life-cycle state machine; KDP and review objects with entrance/success criteria as checkable queries; a baseline snapshot per review | Pre-A–F |
| NPR 7123.1D #2, #11 | Requirement objects (shall, rationale, owner, parent); bidirectional trace; TBD/TBR tracking; change history | A–D |
| NPR 7123.1D #12, EIA-649-2 | IRD/ICD/IDD objects with two-sided ownership and signatures; ICWG change flow; interfaces as code with schema versioning | A–E |
| NPR 7123.1D #14, EIA-649C/-2 | CIs; functional, allocated and product baselines; ECR/ECP/CCB; deviations and waivers; effectivity; FCA/PCA | B–F |
| NPR 7123.1D #15 | Data catalog with metadata, access control, retention and markings; content-addressed artifacts | All |
| SP-6105; NPR 7123.1D #7–8 | VCRM: requirement → TAID method → level → event → closure evidence; validation against ConOps and MOEs | B–E |
| NASA-HDBK-1009A | Model covering MOE, MOP, TPM, ConOps and V&V; generated tables and diagrams | Pre-A–D |
| NASA-STD-7009B / HDBK-7009B | Analysis record (§2); eight-factor credibility; use statement; limits-of-use check; AI-model credibility | A–E |
| NPR 7150.2D SWE-020/052/079/080 | Software class per CSCI; RMM compliance; trace; CM | A–F |
| SWE-136 / SWE-070 | Tool accreditation records for FreeCAD, solvers, MCP servers, agents and the EDM itself | A–E |
| NASA-STD-8739.8B / SWE-141 | SA access; hazard ↔ software trace; IV&V data export | A–E |
| NASA-STD-5001B, 5002, 5019A, 5020 | FoS; load-set versions; MS results; fracture classification; joint analyses | B–D |
| NASA-STD-6016C | Material and process per part; allowables pedigree; MUA workflow | B–D |
| GEVS, NASA-STD-7002 and 7012 | Environmental test matrix; TRR packages; as-run data; test–analysis correlation | C–D |
| NPR 8705.4A | Risk class driving tailoring | Pre-A–A |
| NASA-STD-8729.1, NPR 8715.3 | FMEA/CIL and hazard reports linked to design | A–D |
| NPR 8735.2C, AS9100D, AS9102 | GMIP holds; FAI; NCR/MRB; as-built by serial; traveler linkage | C–D |
| NASA-STD-8739.10, NPR 8735.1D | EEE parts list; GIDEP where-used query | C–E |
| NPR 2810.1F, 800-171, FedRAMP | Access control, audit, encryption, authorized hosting | All |
| NPR 2190.1C | Export classification per object; US-person enforcement; egress controls on agents and LLMs | All |
| NASA GenAI guidance / M-25-21 | AI provenance on every artifact (model, version, context hash, approver); approved-tool registry | All |
| Contract DRDs | DRD-conformant documents rendered from the model; submittal and approval status | Per contract |

## 9. Design implications

1. **Tailoring is data.** Store the compliance matrices themselves as first-class objects:
   - the 7123.1 App H matrix;
   - the 7150.2 RMM;
   - 7009B tailoring;
   - the DRD list.

   Key them by risk class and software class, and record TA approvals. Every gate derives from
   them.
2. **A baseline is a signed git ref plus a graph snapshot.** The entrance criteria for each review
   are executable queries against that snapshot.
3. **Human approval is the legal act.** Agents write only to proposal branches. A merge into a
   controlled baseline requires a signed CCB, ICWG, MRB or TA disposition from a named, qualified
   human.
4. **Every FEA and thermal run carries a 7009B analysis record.** An MS result cannot close a VCRM
   row below the tailored credibility threshold.
5. **Keep a tool accreditation registry.** Every attestation cites an accredited tool version. A
   version change invalidates the results that depend on it.
6. **Interfaces are code with two-sided ownership.** The trace runs IRD → ICD → verification, and
   the ICD DRDs are generated.
7. **Link as-designed to as-built.** Track serials, lots, travelers, FAI, NCR/MRB, GMIPs and GIDEP
   where-used.
8. **Markings and export control apply per object** and propagate through derivations. Use a private
   Sigstore and US-person-operated hosting.
9. **Attach AI provenance to every artifact.**
10. **Generate documents; don't author them.** Keep a SysML v2 ↔ HDBK-1009A metamodel mapping.

## 10. Verify before relying on this note

- Whether NID 7120.148 was extended, or NPR 7120.5G issued.
- The exact text of 7009B and HDBK-7009B, including whether the eight factors were restructured.
- Whether NPR 8705.4B is in effect.
- Whether any NASA engineering-AI NPR exists.
- The current revisions of NASA-STD-5020, 7002 and 8739.10, and of NPD 8730.5.
