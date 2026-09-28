# DoD requirements → EDM obligations (research note)

- **Research date:** 2026-09-28.
- **Part of:** the EDM plan (`docs/edm-plan.md`).
- **Method:** the research environment's proxy blocked every primary source that was tried
  (esd.whs.mil, cto.mil, media.defense.gov, everyspec). Revisions and dates were therefore
  confirmed through search results and secondary sources: law firms, ANSI, SAE, DAU and the trade
  press.
- **Confidence tags:**
  - **[V]** confirmed in at least one search result.
  - **[P]** paraphrase of a policy text that could not be opened; check against the source before
    quoting.
  - **[U]** unverified.

## 0. Findings that change the plan

1. **An AI supplier restriction on DoW work.**
   - On 2026-03-03 the Department of War (DoW, the renamed DoD) designated Anthropic a
     supply-chain risk under both 10 USC 3252 and 41 USC 4713 (FASCSA).
   - The DC Circuit upheld the FASCSA designation on 2026-09-25 (No. 26-1049). A separate
     California ruling struck the §3252 label.
   - As reported, the designation bars use of Claude by the military and by contractors in DoW
     contract work. It does not bar non-DoW uses. [V, from the trade and legal press]
   - Sources:
     <https://www.cnbc.com/2026/09/25/pentagon-anthropic-ai-risk-appeals-court.html>,
     <https://www.mayerbrown.com/en/insights/publications/2026/03/anthropic-supply-chain-risk-designation-takes-effect--latest-developments-and-next-steps-for-government-contractors>,
     <https://www.goodwinlaw.com/en/insights/publications/2026/03/alerts-practices-is-claude-a-supply-chain-risk>
   - **Implications for the EDM:**
     - The agent/MCP layer must be **model-provider-agnostic**.
     - The LLM provider and model are **configuration items** and are checked against a
       program-specific **allow-list** of models, which applies supplier exclusions.
     - *Contracts and legal must confirm what applies to each program before any AI tooling
       touches DoW work.* This includes the tooling used to write the EDM itself.
2. **CMMC Phase 2 is suspended.**
   - Third-party (C3PAO) Level 2 assessments were due to start 2026-11-10. DoW suspended Phase 2
     on 2026-07-13 and set up a Reform Task Force, whose report was due 2026-09-11 and is not yet
     public. [V]
   - Phase 1 self-assessments, DFARS 252.204-7012 and NIST SP 800-171 **r2** still apply.
   - Sources:
     <https://federalnewsnetwork.com/cybersecurity/2026/07/pentagon-suspends-cmmc-phase-two-requirements-launches-review-of-program/>,
     <https://www.nextgov.com/acquisition/2026/09/cmmcs-phase-2-suspension-locked-binding-regulation/415890/>
3. **RMF → CSRMC.**
   - DoD announced the Cybersecurity Risk Management Construct on 2025-09-24. It has five phases
     (Design, Build, Test, Onboard, Operate), with continuous monitoring and a "constant ATO"
     posture. [V]
   - DoDI 8510.01 (Jul 2022) remains the formal issuance.
   - Source: <https://breakingdefense.com/2025/09/dod-issues-replacement-for-risk-management-framework/>
4. **There is no single "DoD cert".**
   - No certification applies to an EDM as such. What exists instead:
     - **program-level technical certifications**: airworthiness (DoDI 5030.61, MIL-HDBK-516C),
       cyber ATO, safety and interoperability;
     - **company-level certifications**: CMMC, AS9100, NADCAP, and purchasing-system approval
       under DFARS 252.246-7007.
   - The EDM's job is to **produce the evidence for each of these cheaply**.
   - See the OUSD(R&E) Acquisition Program Technical Certifications Summary (Jul 2024):
     <https://www.cto.mil/wp-content/uploads/2024/08/AcqTechCert-Summary-July2024.v2.pdf>

## 1. Digital engineering and acquisition policy

**DoDI 5000.97, "Digital Engineering" (2023-12-21)** [V]:
- New programs must use digital engineering (DE); existing programs adopt it where practicable.
- Programs weigh how practical and beneficial DE is, and put **digital models and data on
  contract as deliverables**.
- Programs reuse existing DoD DE resources.
- Detailed models are the primary way system information is communicated.
- The DE capability covers "development, verification, validation, use, **curation**,
  **configuration management**, and maintenance" of models.
- The digital thread and an **authoritative source of truth (ASOT)** provide traceability
  across configuration-controlled model versions.
- Data and IP rights are planned for and acquired, and DE appears in the SEP, TEMP and LCSP. [P]
- Source: <https://www.esd.whs.mil/Portals/54/Documents/DD/issuances/dodi/500097p.PDF>

**2018 DE Strategy** [P] has five goals:
1. formalize models;
2. provide an enduring ASOT;
3. incorporate innovation;
4. provide infrastructure and environments;
5. transform culture and the workforce.

**SysML v2:**
- OMG final adoption came on 2025-07-21. [V]
- OUSD(R&E) published SysML v2 transition guidance in a Feb 2026 info sheet. [V]
  <https://www.cto.mil/wp-content/uploads/2026/02/SysML-Info-Sheet-Feb2026.pdf>
- This supports the plan's choice of SysML v2 with Flexo MMS.

**DoDI 5000.88, "Engineering of Defense Systems" (Nov 2020)** [V]:
- Programs hold SRR/SFR, PDR, CDR, SVR/FCA, PRR and PCA unless the SEP waives them.
- OUSD(R&E) assesses PDR and CDR for ACAT ID programs.
- Entrance and exit criteria come from the DoD SE Guidebook (Feb 2022):
  <https://ac.cto.mil/wp-content/uploads/2022/02/Systems-Eng-Guidebook_Feb2022-Cleared-slp.pdf>
- What each review establishes:

  | Review | Establishes |
  |---|---|
  | SRR/SFR | Functional baseline |
  | PDR | Allocated baseline |
  | CDR | Initial product baseline |
  | SVR/FCA | Verified performance |
  | PCA | As-built conforms to the product baseline and the TDP |

**Review standards:**
- IEEE 15288.1 and 15288.2 are **inactive**.
- DoD now cites **ISO/IEC/IEEE 24748-7:2019** (SE on defense programs) and **24748-8:2019**
  (technical reviews and audits). [V]
- The base standard is ISO/IEC/IEEE 15288:2023.

**DoDI 5000.89, "Test and Evaluation"** [V]:
- It introduces the **Integrated Decision Support Key (IDSK)**, a table that maps decisions to
  evaluation areas, critical issues and test artifacts.
- The cyber DT&E counterpart is DoWM 5000.103.

**Acquisition pathways:**
- MTA is governed by 5000.80, Major Capability by 5000.85 and Software by 5000.87.
- A 2025-03-06 memo made the Software Acquisition Pathway, CSOs and OTAs the default for
  software. [V]
- A 2025-11-07 memo introduced Portfolio Acquisition Executives. [V]
- **Implication:** expect fewer gate documents and more continuously queried evidence.

## 2. Standards and data items

**Technical data packages (TDP):**
- **MIL-STD-31000B (2018-10-31)** is current, and **no revision C was found**. An additive
  manufacturing revision was targeted for 2024 but no evidence of its issue was found. [V]
- Do not confuse it with MIL-DTL-31000C, which is cancelled.
- The drawings and models DID is **DI-SESS-81000**. [V]

**WBS:** MIL-STD-881F (May 2022). [P]

**Specifications:**
- MIL-STD-961E with Change 4 (2020-07-16). [V]
- Section 4 of a spec is Verification. No VCRM-specific DID was found; the VCRM is normally the
  Section 4 table. [U]

**System safety:**
- MIL-STD-882E **with Change 1 (2023-09-27)**. [V]
- Hazard mitigations become derived requirements.
- Relevant DIDs: DI-SAFT-80101 (HAR), 80102 (SAR), 81300 (MRAR) and 81626 (SSPP). [V]

**FMECA:**
- DID DI-SESS-81495 (revision C). [V]
- MIL-STD-1629A is cancelled [U]; industry uses SAE ARP5580. [U]

**Configuration management:**
- Governing documents: MIL-HDBK-61B (2020-04-07) [V], SAE EIA-649C (2019) [V], and **SAE
  EIA-649-1A (Aug 2020)**, which carries the defense contract requirements. [V]
- Relevant DIDs:
  - CM Plan: DI-CMAN-80858 or DI-SESS-80858C;
  - ECP: **DI-SESS-80639**;
  - Configuration Audit Plan (FCA/PCA): **DI-SESS-81646**. [V]

**SE and interface DIDs** [V]:
- SEMP: **DI-SESS-81785A**.
- IRS: DI-IPSC-81434(A).
- IDD: DI-IPSC-81436(A).
- Test/Inspection Report: **DI-NDTI-80809B**.
- Test Procedure: **DI-NDTI-80603A**.

**Environmental and structural** [V]:
- MIL-STD-810H with Change 1.
- **MIL-STD-461H (2026-04-17) supersedes 461G.**
- MIL-STD-1530D with Change 1 (ASIP).
- **SMC-S-016 (2014-09-05)**, the space test requirements, is active and replaces MIL-STD-1540.

**Markings and control** [P]:
- DoDI 5230.24: distribution statements.
- DoDI 5200.48: CUI.
- DoDI 5000.83: program protection.
- DFARS 252.227-7013, -7014 and -7017: data rights.

## 3. Quality and manufacturing

**AS9100D** is current. Its successor, **IA9100**, is expected Q4 2026 to Q1 2027 alongside ISO
9001:2026, with a transition of about three years. [V]

**Inspection and production:**
- **AS9102C (2023):** FAI now addresses 3D model data. [V]
- **AS9145:** APQP/PPAP. [V]
- **AS9103:** key characteristics. [U: revision not confirmed]

**Counterfeit avoidance:**
- AS5553 rev E [V weak]; AS6174 [U].
- **DFARS 252.246-7007** requires part traceability "from the original manufacturer to product
  acceptance". [V]
- **DFARS 252.246-7008** covers sources of supply. [V]

**Workmanship:**
- J-STD-001J and IPC-A-610J (2024).
- A Space/Military Addendum to J-STD-001J was released in Jan 2025. [V]
- NADCAP accredits special processes. [P]

## 4. Cyber and infosec for the toolchain itself

- **CUI:**
  - DFARS 252.204-7012 requires NIST SP 800-171 **r2** (class deviation 2024-O0013). [V]
  - Cloud services must be FedRAMP Moderate equivalent. [P]
  - Incidents are reported within 72 hours. [P]
  - The FAR CUI rule was re-proposed on 2026-06-23. [V]
- **CMMC:**

  | Level | Scope | Assessment |
  |---|---|---|
  | L1 | 15 requirements | — |
  | L2 | 110 requirements (800-171 r2) | Self-assessment or C3PAO |
  | L3 | L2 plus 24 requirements from 800-172 | DIBCAC |

  This breakdown is a paraphrase [P]. For phase status, see §0.
- **ITAR, 22 CFR 120.54(a)(5):**
  - Encrypting data end to end with FIPS 140-validated crypto, with no third party holding the
    keys, means storing or transferring it is not an export. [V]
  - **That exemption does not cover an LLM,** because the model has to see plaintext. ITAR data
    therefore needs US-person-operated enclaves, such as GovCloud or Azure Government.
  - GitHub Enterprise Cloud US data residency (GA May 2025) is **not** an ITAR enclave.
    GitHub Enterprise Server on GovCloud is the typical choice. [V/P]
- **RMF and continuous ATO:**
  - DoDI 8510.01 and NIST 800-37 r2. [V]
  - **NIST 800-53 Release 5.2.0 (2025-08-27)** adds SA-15(13), **SA-24** and SI-02(07) on update
    and patch integrity. [V]
  - Also relevant: the cATO evaluation criteria, the DevSecOps Fundamentals, CSRMC, Iron Bank /
    Platform One images, and DISA STIGs.
- **Software supply chain:**
  - EO 14028 established SBOMs. EO 14306 (2025-06-06) **removed the FAR attestation mandate**.
    [V]
  - SSDF SP 800-218r1 was published as an initial public draft (IPD) on 2025-12-17. [V]
  - **DoD SWFT (2025):** vendor SBOM plus a third-party SBOM and assessment, uploaded into eMASS.
    [V] <https://defensescoop.com/2025/06/09/katie-arrington-swft-software-fast-track/>
  - SLSA and in-toto are not mandated, but they map directly to SA-24 and to the SWFT evidence. [P]

## 5. AI assurance

- **DoW AI Strategy (Jan 2026)** [V]:
  - The posture is "AI-first".
  - "Responsible AI" is redefined as "objectively truthful AI … employed securely and within the
    laws".
  - Contracts allow "any lawful use", and new models must be available within 30 days.
  - MOSA and the "Data Decrees" are enforced.
  - GenAI.mil is authorized at IL5 for CUI.
  - Source: <https://www.defenseone.com/policy/2026/01/grok-ethics-are-out-pentagons-new-ai-acceleration-strategy/410649/>
- **Earlier guidance still on the books:** the RAI Strategy and Pathway (2022/2024) and the CDAO
  RAI Toolkit were not rescinded but are de-emphasized. [V]
- **OMB memos** [V]:
  - M-25-21 and M-25-22 (2025-04-03) replaced M-24-10 and M-24-18.
  - **M-26-04 (2025-12-11)** requires "truth-seeking" and "ideological neutrality" clauses in LLM
    contracts.
- **NIST:** AI RMF 1.0 and **AI 600-1**, which lists 12 generative-AI risks including confabulation
  and value-chain integrity. [V]
- **DoDD 3000.09** applies only if the product being engineered is an autonomous weapon system.
- **No DoD rule is specific to AI-authored engineering artifacts.** The expectations follow from
  existing rules [P]:
  - EIA-649-1A change control by an accountable human;
  - AS9100 §7.5 and §8.3;
  - VV&A for analytical results;
  - the AI 600-1 provenance and human-AI configuration controls.
- "Agent proposes, human approves" fits all of these.

## 6. Tool qualification and M&S credibility

- **There is no DoD-wide equivalent of DO-330.** MIL-HDBK-516C §15 cites DO-178C and DO-330 for
  airborne software tools. [V]
- **FEA and thermal tools become credible through VV&A for an intended use:**
  - **DoDI 5000.61 (reissued 2024-09-17):** risk-based VV&A across the lifecycle, with emphasis on
    uncertainty quantification and model maturity. [V]
  - **DoDM 5000.102 (2024-12-09):** M&S must be accredited before record runs for OT&E and LFT&E.
    [V]
  - **MIL-STD-3022 with Change 1:** four products are required (Accreditation Plan, V&V Plan,
    V&V Report and Accreditation Report). [V]
  - DAU's M&S for T&E Guidebook (May 2025). [V]
- NASA-STD-7009B is a useful credibility rubric for DoD work, but DoD does not require it.

## 7. Mapping: requirement source → EDM obligation → phase

| Source | EDM must provide | Phase |
|---|---|---|
| DoDI 5000.97; DE Strategy | Versioned models in the ASOT (git + Flexo) with curation status and owner; the model set at any baseline; models and data as CDRLs | All |
| 24748-7/-8; SE Guidebook; 5000.88 | Review objects (SRR…PCA) with machine-checked entry and exit criteria; a review package frozen at a tag; baseline promotion only at review exit | Reviews |
| EIA-649-1A; MIL-HDBK-61B | CIs; functional, allocated and product baselines; ECP (DI-SESS-80639); deviations and waivers; status accounting query; FCA/PCA (DI-SESS-81646) | EMD → sustainment |
| MIL-STD-961E; DI-IPSC-81434/81436 | Requirements with verification method (I/A/D/T); interfaces as code generating IRS and IDD; VCRM; orphan and unverified query | SRR → SVR |
| MIL-STD-881F | WBS element on every CI and task | All |
| MIL-STD-882E Chg 1 | Hazards linked to mitigating requirements and verifications; open hazards and acceptance authority; DI-SAFT-80101/80102 output | All |
| DI-SESS-81495 | FMECA failure modes linked to parts, functions, detection and the CIL | PDR → CDR |
| MIL-STD-31000B; DI-SESS-81000; AS9102C | MBD with 3D PMI, STEP AP242, TDP index with rights and distribution markings; FAI linked to the model revision | CDR → PRR |
| 810H, 461H, SMC-S-016, 1530D; DI-NDTI-80603/80809 | Test procedures and reports as data; links from environments to requirements; test–analysis correlation | TRR → SVR |
| 5000.89 IDSK | Query: decision → critical issue → test artifacts | DT/OT |
| 5000.61; DoDM 5000.102; MIL-STD-3022 | M&S registry (tool, version, settings, mesh, intended use, UQ); the four MIL-STD-3022 products; results not usable for record until accredited | PDR → OT&E |
| AS9100D/IA9100; AS9145; AS9103 | Design review and V&V records; key characteristics flagged on the model and flowed to control plans | CDR → production |
| DFARS 252.246-7007/-7008; AS5553 | Part pedigree per as-built serial | Production |
| DFARS 252.227-7013/-7017; DoDI 5230.24 | Rights assertion, legend and distribution statement per artifact; gates on export and delivery | All |
| DFARS 7012; 800-171 r2; CMMC; ITAR | CUI/ITAR enclave, US-person access, FIPS 140 crypto, audit, 72-hour reporting; labels on every node | All |
| 8510.01/CSRMC; 800-53 r5.2; SWFT; SSDF | SBOMs, SLSA/in-toto provenance, STIG'd Iron Bank images, continuous monitoring | Toolchain |
| AI Strategy 2026; M-26-04; AI 600-1; FASCSA | AI provenance record per proposal (model, version, context hash, tools, approver); model allow-list with supplier exclusions | All |

## 8. Design implications

1. **Baselines are signed, first-class objects.** A baseline is promoted only at review exit, and
   status accounting is a query.
2. **Put a CM layer over git.** It holds CIs, ECP/deviation/waiver objects, and the Class I/II
   change classification.
   - **A merge implements an approved ECP; it is not the approval itself.**
3. **Attach classification, rights and export metadata to every node.** Enforce them in:
   - Flexo queries;
   - the store;
   - the builder that assembles an agent's context.
4. **Run Sigstore privately.** Use FIPS-validated crypto and a transparency log hosted inside the
   enclave.
5. **LLM providers are supply-chain items.** Keep a model allow-list, run inference in ITAR
   enclaves, and keep the MCP layer provider-agnostic.
6. **Every agent proposal carries an AI-provenance attestation.** It is an in-toto predicate, and
   the human approver maps to the CM approval authority.
7. **Keep an M&S registry and a VV&A workflow.**
   - Results from an unaccredited tool cannot close a requirement.
   - The MIL-STD-3022 products are generated.
8. **Verification is data.** A live VCRM and IDSK, with hazards and FMECA linked into the graph.
9. **The product-definition path must be MBD-native:**
   - AP242 with PMI;
   - key-characteristic flags;
   - TDP export that passes 31000B and DI-SESS-81000;
   - AS9102C FAI;
   - as-built pedigree under DFARS 7007/7008.
10. **The toolchain itself must be authorizable:**
    - Iron Bank / STIG'd containers;
    - an SBOM plus SLSA provenance for each release (SWFT, SA-24);
    - cATO/CSRMC monitoring;
    - 800-171 r2 now, with r3 as the target;
    - a GovCloud-class enclave;
    - no public GitHub for ITAR or CUI.

## 9. Gaps to close

- Read the DoDI 5000.97 §3–4 text directly.
- Confirm the revisions of AS6174, AS9103 and AS9145.
- Confirm that MIL-STD-1540 is cancelled and find the Aerospace TOR number.
- Watch for the CMMC Task Force report (expected late Sep to Oct 2026) and the IA9100 publication.
- **Contracts and legal:** confirm how the FASCSA designation applies to each program, including to
  the development tooling.
