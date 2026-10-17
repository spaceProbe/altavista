# spoore upstream proposals

AltaVista depends on spoore's CDM crate by path (question 12: spoore crates by dependency,
CDM v1 a compatible superset, changes spoore needs go upstream as PRs). Question 75 recorded
three changes that spoore should carry itself. They are now implemented as three commits on
the `altavista-upstream` branch of the spoore checkout at `/Users/probe/code/spoore`, one
commit per proposal so each can go up as its own PR.

| Commit | Proposal (question 75) | Files | Wire or behaviour change |
|---|---|---|---|
| `ea19659` | `spoore-cdm/build.rs` generates `BTreeMap` for every proto `map<...>` field (`prost_build::Config` with `.btree_map(["."])`), so `Measurement.meta` encodes in key order instead of `HashMap`'s per-process random order. spoore's ADR-004 already forbids unordered iteration on any output path and names `Measurement::meta` as the example. | `crates/spoore-cdm/build.rs` | Wire bytes become deterministic; no call site changed. |
| `ccf8d23` | `spoore/v0/cdm.proto` states the time scale of `epoch_ns` on `TrackSeed`, `TrackHandoff` and `TrackUpdate` ("Event time, nanoseconds since the Unix epoch."), matching the comment `GaussianState` and `Measurement` already carry and `spoore_cdm::state::Epoch`'s own doc. | `proto/spoore/v0/cdm.proto` | Comment only; no renumbering, no wire change. |
| `9d732cc` | `spoore_cdm::state::SYMMETRY_RTOL`, `validate_covariance` and `check_positive_definite` become `pub` with doc comments and are re-exported from `spoore_cdm`, so a downstream crate can apply spoore's canonical covariance check (same checks, same order, same tolerance) to a matrix that is not yet a `GaussianState`. | `crates/spoore-cdm/src/state.rs`, `crates/spoore-cdm/src/lib.rs` | Visibility only. |
| `fca6323` | Every workspace crate declares `publish = false` (question 207, 2026-09-13). The crates are consumed by path dependency only; cargo-deny's wildcard ban exempts path dependencies only from crates declared private, so a downstream workspace that bans wildcard versions sees six wildcard errors from spoore's own inter-crate path dependencies until this is declared. | `crates/*/Cargo.toml` (eleven files) | Metadata only. **Blocks a downstream gate:** AltaVista's `deny.toml` runs `wildcards = "warn"` instead of `"deny"` until this lands on spoore's `main`. |

A fifth proposal is pending as prose, not yet a commit: the consumer-trait shape `av-track`
needs from spoore-io, described in `crates/av-track/src/consumer.rs`'s module documentation.

## Sixth proposal (text only, question 246): spoore-tree off `serde_yaml`

**Why.** `serde_yaml` 0.9 is deprecated and its parser `unsafe-libyaml` 0.2.11 carries
`RUSTSEC-2024-0449` (unmaintained, repository archived, no safe upgrade). AltaVista moved all of
its own YAML reading (`av-kernel`, `av-sweep`, `av-command`, `av-gateway`, `av-jobs`) to
`crates/av-yaml` over `serde-saphyr` (question 246). `spoore-tree` is now the only crate in the
AltaVista build that still reaches `serde_yaml` (path: `av-track` -> `spoore-engine` ->
`spoore-tree`; also `av-track` directly), so AltaVista's `deny.toml` keeps one named ignore for
`RUSTSEC-2024-0449` and a `[bans]` rule that allows `serde_yaml` only as a direct dependency of
`spoore-tree`. When this proposal lands the ignore and that ban entry are deleted in one commit.

**What.** `crates/spoore-tree` (workspace dependency `serde_yaml = "0.9"`) moves to
`serde-saphyr` 2 (MIT OR Apache-2.0, `forbid(unsafe_code)`, no `unsafe-libyaml`; build it with
`default-features = false, features = ["deserialize"]`). It needs MSRV 1.89.

- **Parsing** (`src/config.rs` lines 381, 390, 1108, `serde_yaml::from_str`): `serde_saphyr::from_str_with_options`
  with `strict_booleans: true`, `duplicate_keys: Error`, `merge_keys: Error`,
  `reject_unsupported_tags: true`. These options make it read YAML 1.1 words (`yes`, `no`, `on`)
  as strings and refuse duplicate keys, merge keys and tags, as `serde_yaml` did or refused.
- **The public API that names `serde_yaml` types** has to change, because `serde-saphyr` has no
  `Value`/`Mapping` and no `from_value`: `ConfigError::Parse { source: serde_yaml::Error }` and
  `Params(#[from] serde_yaml::Error)` (lines 103, 118, 880), `ModelSpec.params: serde_yaml::Mapping`
  (line 172), and the factory signatures that take a `serde_yaml::Value` and call
  `serde_yaml::from_value` (lines 863-914, 1238-1290, 1339, 1389, 2039). The smallest change is
  `serde_json::Value` (with `serde_json`'s `preserve_order` feature if `params` order is observable)
  for the untyped params and `serde_json::from_value` for the typed step, and a spoore-owned
  error type (a `String` message plus line and column) in place of `serde_yaml::Error`. This is a
  breaking change to `spoore-tree`'s public types; AltaVista's `av-track` names none of
  them (it uses `ModelTree`, `ModelNode` and `NodePolicy` only).
- **Dialect differences to decide on, not to inherit silently.** `serde-saphyr` reads some plain
  scalars as other values than `serde_yaml` did (`nULL` and `tRUE` as null and boolean,
  `1_000` as an integer, `017` as a number, `0O7` as an integer, an integer past 128 bits or
  `1e400`), appends a `\n` to a block scalar that ends an input with no final newline, keeps
  U+0085/U+2028/U+2029 as characters where `serde_yaml` read line breaks, refuses a null for a
  `String` field (`serde_yaml` gave `""` or the text), and refuses continuation lines of a
  multi-line quoted scalar that are not indented past the parent. AltaVista's `av-yaml` refuses
  the first four groups with a typed error so no file changes value; its measured table is the
  crate documentation of `crates/av-yaml/src/lib.rs` in AltaVista, and its `scan.rs` and
  `dialect.rs` are about 450 lines with their tests that spoore can copy or reimplement. Spoore's own fixtures
  and `terrain/` and `trees/` YAML must be checked against whichever policy spoore chooses.

**Verification to run in spoore.** `cargo build --workspace --all-features --all-targets` and
`cargo test --workspace --all-features --no-fail-fast` against the 1090 passed / 25 ignored
baseline; `cargo xtask goldens --check` and `cargo xtask ledger --check` unchanged; then, in
AltaVista with the new spoore checked out, `cargo test -p av-track`, `cargo build --workspace`,
and `cargo deny check` with the `RUSTSEC-2024-0449` ignore and the `serde_yaml` ban entry
removed (it must pass).

## Verification

Each commit was checked in the spoore workspace: `cargo build --workspace --all-features
--all-targets` and `cargo clippy --workspace --all-features --all-targets` clean, `cargo test
--workspace --all-features --no-fail-fast` 1090 passed / 25 ignored at every step, identical to
the pre-change baseline, and `cargo xtask ledger --check` and `cargo xtask goldens --check`
passing after the third commit. With the branch checked out as AltaVista's path dependency,
`cargo test -p av-cdm` passes 47/47 unchanged.

## What AltaVista does until the PRs land

- `av-cdm` keeps its own covariance check, proven equivalent to spoore's in M4.2 (question 75).
  Replacing it with the re-exported spoore functions is a one-line change per call site once
  the third commit is on spoore's main; doing it earlier would make AltaVista build only
  against this branch.
- The path dependency follows whatever branch the spoore checkout has out. The branch is a
  superset of main at the source level (no call site changed, no field renumbered), so
  AltaVista builds and tests identically against either.
- `Measurement.meta` ordering only matters for spoore's own encoding path. AltaVista's
  `spoore.v0` adapter tests in `crates/av-cdm/tests/spoore_v0.rs` are unaffected either way.

## Submitting

Open the three PRs from `altavista-upstream` in the order above (each is independent; the
order only matches the numbering in question 75). Commit bodies carry the rationale and the
measured gate counts, and reference spoore's ADR-004 where it applies. No AltaVista change is
needed when they merge; record the merge in question 75 and delete the branch.
