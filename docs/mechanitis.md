# Mechanitis (engineering data model): relationship to AltaVista

The engineering data model (EDM) plan was drafted here and moved on 2026-09-28 to its own
repository, **`spaceProbe/Mechanitis`**. The plan is `docs/plan.md` there. The same repository
holds:

- the DoD/NASA compliance crosswalk;
- the tool-module contract, with FreeCAD as module #1;
- the research notes.

## What Mechanitis takes from AltaVista

- **Core types:** `Provenance` (including `AuthorKind.AGENT`), `AssetRef`, `Label`, `Unit`, and
  TAI-nanosecond time. `edm.v1` imports them from `altavista.v1` until Mechanitis Q-E1 decides
  whether to extract a shared `common.v1`.
- **Hashing and schema rules:** SHA-256 over the deterministic encoding with the `hash` field
  cleared, strict `buf breaking`, and a YAML mirror of the proto.
- **Patterns, not code (yet):**
  - `av-store`: content-addressed, with label check before fetch and verification after;
  - `av-catalog`;
  - the hash-chained evidence log (`evidence.py` / `evidence.rs`);
  - the `av-gateway` MCP pattern: deny-by-default, OIDC, label-aware, propose-only;
  - the `av-command` propose/authorize ledger with Rego via `regorus`;
  - the `IMAGE_DIGEST.md` convention;
  - the goldens rule: a miss records the residual, never a loosened tolerance.

## What AltaVista provides to Mechanitis

- **Validation evidence.** Design reference missions act as validation cases. DRM `Objective`s
  map to validation constraints. `RunProducts` and `ScoreResult` are consumed as evidence by
  hash.
- **Interface control parameters.** `SystemDefinition`, `Port` and `Parameter` are generated
  from, or checked against, the Mechanitis SysML v2 model. `Port.schema`, `interface_class` and
  `port_traffic_hash` are the data-interface control parameters and the in-operation observation
  source.
- **Thermal boundary conditions.** Trajectories and eclipse events feed orbit-thermal boundary
  conditions for the Mechanitis `thermal-elmer` module.

Nothing in AltaVista changes because of this until the Mechanitis ADRs are accepted. Changes
AltaVista would need then will be proposed the way `docs/spoore-upstream.md` does it.
