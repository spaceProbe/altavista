//! Role-gated authorization, MFA for hazardous classes, and time-limited delegations (A2.2,
//! `docs/aiplane-plan.md` milestone A2's second half; ADR-004's "Command authority" section;
//! `docs/open-questions.md` question 54: "role-gated authorization per command class ...
//! time-limited delegations with enforced expiry"; question 34: `groups` for roles, `amr`/
//! `acr` for MFA). [`authorize_command`] is the one entry point `crate::service` calls from
//! `Authorize`, after `crate::oidc::verify` has already established *who* the caller is
//! (A2.1) -- this module decides *whether* that caller may authorize a given [`Command`].
//!
//! # Deny by default, made structurally hard to get wrong
//!
//! Three failure shapes this task's own brief names as the ones most likely to ship broken,
//! and how this module closes each:
//!
//! 1. **"A role table that treats 'no table configured' as 'allow'."** [`RoleTable::
//!    granting_role`] returns `Option<&str>`; an empty table (no `roles:` block, or a
//!    profile that declares the block with no entries) makes every lookup return `None` for
//!    every class, which [`authorize_command`] turns into [`AuthzError::RoleNotGranted`] --
//!    there is no code path in this module that starts from "allow" and narrows; every path
//!    starts from "no grant found" and only a real match flips it.
//! 2. **"A delegation whose expiry is checked with the wrong comparison."** The boundary
//!    check is `now_tai_ns >= delegation.expires_tai_ns` (expired, refused) -- copied,
//!    deliberately, from `crate::oidc::TokenError::Expired`'s identical `now_tai_ns >=
//!    exp_tai_ns` -- so this crate has one expiry-boundary convention, not two that could
//!    drift apart (see `authority.proto`'s `Delegation` doc comment, which states the same
//!    rule at the schema level). [`tests::delegation_expiry_is_refused_at_the_boundary_
//!    second_one_nanosecond_earlier_is_not`] pins both sides of the boundary with a
//!    [`crate::clock::TestClock`] value, never a sleep.
//! 3. **"An MFA check that passes when the `amr` claim is simply absent."** The MFA check
//!    (see below) is `mfa_amr_methods.iter().find(|m| principal.amr.contains(m))` --
//!    `Vec::contains` over an *empty* `principal.amr` is `false` for every `m`, so an absent
//!    `amr` claim can never satisfy this by accident; there is no default-true branch and no
//!    `.unwrap_or(true)` anywhere in this module. [`tests::hazardous_with_an_empty_amr_and_no_
//!    configured_acr_is_refused`] exercises exactly the empty-`amr` case directly.
//!
//! # Role gate
//!
//! [`RoleTable`] wraps a `Vec<`[`av_cdm::pb::RoleBinding`]`>` (the profile's declared table,
//! `roles:` inside `profiles/execution.yaml`'s `authority:` block -- see
//! [`load_profile_authz_config`]) built from a `BTreeMap<String, Vec<String>>` (never a
//! `HashMap`: ADR-004's determinism rule, and this table's own iteration order therefore
//! never varies run to run). A principal may authorize command class `C` only if at least one
//! of `Principal.groups` (claim order, per `crate::oidc`'s own convention -- this module never
//! re-sorts it) is a role in the table whose `command_classes` lists `C` or the wildcard
//! [`WILDCARD`] (`"*"`). The **first** such group, in claim order, is what gets recorded as
//! the granting role (`AuthorizedVia::Role`) -- deterministic given a deterministic token,
//! never "whichever the table happens to iterate to first".
//!
//! # MFA gate
//!
//! Required only when `Command.hazardous` is `true` (`command.proto`'s own doc comment: "may
//! require additional authorization"). Two checks, in this order, either one sufficient:
//!
//! 1. **`amr` containment (primary)**: `principal.amr` contains at least one of the profile's
//!    `mfa_amr_methods`. This is the check this module implements as primary, because
//!    secsso's own MFA contract (question 34: "MFA via `amr`/`acr`") makes `amr` -- the
//!    Authentication Methods Reference, RFC 8176, a set of methods that actually ran -- the
//!    more direct signal: it is a plain set-membership fact ("did an OTP step run"), with no
//!    ordering or hierarchy to get wrong, unlike `acr`.
//! 2. **`acr` exact match (supported, not primary)**: `principal.acr` equals the profile's
//!    configured `mfa_acr` string (when that string is non-empty -- an empty configured
//!    `mfa_acr`, like an empty `principal.acr`, means "not configured"/"not present", never a
//!    wildcard match against each other). This is genuinely cheap to support (the field is
//!    already carried and verified by A2.1) so it is implemented too, but only as **exact
//!    string equality against one configured value** -- OIDC's Authentication Context Class
//!    Reference (RFC 8176) has no platform-independent ordering ("is `acr` X at least as
//!    strong as Y" is an IdP-specific policy question this crate has no way to answer
//!    generically), so this module makes no attempt at a "meets or exceeds" comparison; a
//!    profile that wants `acr`-gated MFA must configure the exact value its IdP asserts for a
//!    successful MFA step. **What is not implemented**: any richer `acr` semantics (a
//!    hierarchy, multiple acceptable values, "compare against a numeric level") -- if a future
//!    IdP's contract needs that, it is a documented, additive extension to
//!    [`ProfileAuthzConfig::mfa_acr`]'s single-string shape, not something this task's brief
//!    asked for ("support the other if it is cheap").
//!
//! Neither claim succeeding is [`AuthzError::MfaRequired`], distinct from
//! [`AuthzError::RoleNotGranted`] (see
//! `tests::hazardous_without_mfa_is_refused_with_a_distinct_reason_from_wrong_role`).
//!
//! # Delegations
//!
//! [`DelegationTable`] is a `BTreeMap<String, `[`av_cdm::pb::Delegation`]`>` keyed by
//! `Delegation.id` (again, never a `HashMap`), loaded once, at process construction, from a
//! profile-declared YAML file ([`load_delegations_file`], `authority.delegations_path`) --
//! **this is the whole storage decision**: delegations are static, reviewed, profile-declared
//! configuration, exactly like the Rego policy bundle A1.2 already loads the same way, not a
//! runtime-mutable store. This task's brief calls a profile-declared file "the obvious
//! minimum," and building a delegation-issuing RPC (a way to *create* one at runtime) is out
//! of this task's scope -- `docs/aiplane-plan.md`'s A2 milestone text asks for "time-limited
//! delegations ... with enforced expiry," which is an *enforcement* property of an existing
//! delegation, not an issuance workflow; nothing in the brief's four named tests or five
//! deliverables asks for one either. A file is also what "deterministic" cashes out to
//! concretely: the exact same bytes on disk produce the exact same [`DelegationTable`] on
//! every load, with no clock, no randomness and no network involved (mirrors
//! [`crate::policy::PolicyBundle::load`]'s own determinism).
//!
//! `AuthorizeRequest.delegation_id` (non-empty) selects a delegation **instead of** the role
//! check, never as a fallback after a role check fails silently into it or vice versa --
//! [`authorize_command`]'s own branch on `delegation_id.is_empty()` makes this an either/or,
//! matching `authority.proto`'s `AuthorizeRequest.delegation_id` doc comment. A delegation is
//! valid only when **every** one of these holds; the first that fails is the refusal, each
//! its own typed [`AuthzError`] variant:
//!
//! - the id names a delegation in the table ([`AuthzError::DelegationNotFound`]);
//! - `Delegation.subject` equals the verified `Principal.sub` ([`AuthzError::
//!   DelegationSubjectMismatch`]);
//! - `Delegation.command_classes` covers `Command.command_class`, directly or via [`WILDCARD`]
//!   ([`AuthzError::DelegationClassNotCovered`]);
//! - `Delegation.entity_ids` covers `Command.entity_id`, directly or via [`WILDCARD`]
//!   ([`AuthzError::DelegationEntityNotCovered`]);
//! - the injected clock is not before `not_before_tai_ns` ([`AuthzError::
//!   DelegationNotYetValid`]);
//! - the injected clock is strictly before `expires_tai_ns` ([`AuthzError::DelegationExpired`]
//!   -- see the module doc's opening section for the exact boundary and why it matches
//!   `crate::oidc`'s).
//!
//! A delegation that passes every check still goes through the MFA gate above if `Command.
//! hazardous` is set -- a delegation grants *authority over the class*, not an exemption from
//! proving MFA, which is a property of the *principal's own session*, not of any grant.
//!
//! # No generic "not authorized", and no `Result`-to-`bool` collapse anywhere on this path
//!
//! Every [`AuthzError`] variant names exactly one failed check, with its own `Display` text
//! (`crate::service`'s module doc explains why this whole family maps to `PERMISSION_DENIED`,
//! distinct from A2.1's `UNAUTHENTICATED`). [`authorize_command`]'s body is a sequence of
//! `if`/`else`/`?` returns over already-owned `bool`s and `Option`s it computes itself --
//! there is no `.ok()`, no `.unwrap_or(false)`, no catch-all `Err(_) => Ok(...)` anywhere in
//! this file; every comparison that decides a refusal is written out and directly testable.

use std::collections::BTreeMap;
use std::path::Path;

use av_cdm::pb::{Command, Delegation, Principal, RoleBinding};
use thiserror::Error;

use crate::counters::Counted;

/// The literal command-class/entity-id wildcard `RoleBinding.command_classes`/`Delegation.
/// command_classes`/`Delegation.entity_ids` all use to mean "every value" -- see
/// `authority.proto`'s own doc comments on those fields for why an empty list and `["*"]` are
/// kept as two distinct, explicit list contents rather than one shape silently meaning the
/// other.
pub const WILDCARD: &str = "*";

/// A profile's declared role table (`profiles/execution.yaml`'s `authority.roles`, loaded by
/// [`load_profile_authz_config`]), as a real `Vec<`[`RoleBinding`]`>` -- the proto message is
/// the actual runtime lookup structure this module walks, not merely a declared, unused shape.
#[derive(Debug, Clone, Default)]
pub struct RoleTable {
    bindings: Vec<RoleBinding>,
}

impl RoleTable {
    /// Builds a table from a `BTreeMap` (role -> its granted command classes) -- the shape
    /// [`ProfileAuthzConfig::roles`] already deserializes into, so this is a pure, total
    /// conversion with no fallible step. `BTreeMap`, never `HashMap` (ADR-004): the resulting
    /// `Vec` is therefore always in the same, sorted-by-role-name order for a given input,
    /// independent of hashing.
    pub fn from_config(roles: &BTreeMap<String, Vec<String>>) -> Self {
        let bindings = roles.iter().map(|(role, classes)| RoleBinding { role: role.clone(), command_classes: classes.clone() }).collect();
        Self { bindings }
    }

    /// `true` only if `role` is present in this table *and* its `command_classes` lists
    /// `command_class` or [`WILDCARD`]. An absent `role` -- including "the table is empty" --
    /// and a present `role` whose list does not mention `command_class` both return `false`;
    /// there is no third case that returns `true` by default.
    fn role_grants(&self, role: &str, command_class: &str) -> bool {
        self.bindings
            .iter()
            .find(|b| b.role == role)
            .is_some_and(|b| b.command_classes.iter().any(|c| c == command_class || c == WILDCARD))
    }

    /// The first of `groups` (in the exact order given -- claim order, per `crate::oidc`'s own
    /// convention) that [`Self::role_grants`] `command_class`, if any.
    pub fn granting_role<'a>(&self, groups: &'a [String], command_class: &str) -> Option<&'a str> {
        groups.iter().map(String::as_str).find(|g| self.role_grants(g, command_class))
    }
}

/// A profile's declared delegations (`authority.delegations_path`, loaded by
/// [`load_delegations_file`]), keyed by [`Delegation::id`] -- `BTreeMap`, never `HashMap`
/// (ADR-004), so [`Self::all`]'s order is always deterministic.
#[derive(Debug, Clone, Default)]
pub struct DelegationTable {
    by_id: BTreeMap<String, Delegation>,
}

impl DelegationTable {
    /// Builds a table directly from already-constructed [`Delegation`]s -- used by
    /// [`load_delegations_file`] internally in spirit (that function builds the same way from
    /// parsed YAML) and, directly, by this crate's own integration test fixtures that need a
    /// table without going through a real file on disk.
    pub fn from_delegations(delegations: impl IntoIterator<Item = Delegation>) -> Self {
        let mut by_id = BTreeMap::new();
        for d in delegations {
            by_id.insert(d.id.clone(), d);
        }
        Self { by_id }
    }

    pub fn get(&self, id: &str) -> Option<&Delegation> {
        self.by_id.get(id)
    }

    /// Every delegation in this table, sorted by id -- used only by this module's own tests
    /// today; kept `pub` because a later console/evidence surface will want to list them.
    pub fn all(&self) -> impl Iterator<Item = &Delegation> {
        self.by_id.values()
    }
}

/// The YAML shape [`load_delegations_file`] parses one delegation entry from -- a plain serde
/// struct, not the generated [`Delegation`] proto type itself (which derives no `serde`
/// impls; see `crates/av-command/src/policy.rs`'s `ProfileAuthorityConfig` for the identical,
/// already-reviewed pattern of parsing YAML into a hand-written struct and only then building
/// the "real" typed value this crate's logic actually consumes).
#[derive(Debug, Clone, serde::Deserialize)]
struct DelegationConfig {
    id: String,
    subject: String,
    #[serde(default)]
    command_classes: Vec<String>,
    #[serde(default)]
    entity_ids: Vec<String>,
    not_before_tai_ns: i64,
    expires_tai_ns: i64,
    #[serde(default)]
    granted_by: String,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
struct DelegationsFile {
    #[serde(default)]
    delegations: Vec<DelegationConfig>,
}

/// Parses a delegations YAML document's text (the file at `authority.delegations_path`) into
/// a [`DelegationTable`]. A missing `delegations:` key parses as an empty list (`#[serde(
/// default)]` on [`DelegationsFile::delegations`]) -- an empty file is a table with no
/// delegations, never a parse error and never a wildcard-grants-everything table (there is no
/// such shape here at all: an empty table's [`DelegationTable::get`] returns `None` for every
/// id, which [`authorize_command`] turns into [`AuthzError::DelegationNotFound`]).
pub fn load_delegations_file(yaml: &str) -> Result<DelegationTable, serde_yaml::Error> {
    let file: DelegationsFile = serde_yaml::from_str(yaml)?;
    let delegations = file.delegations.into_iter().map(|d| Delegation {
        id: d.id,
        subject: d.subject,
        command_classes: d.command_classes,
        entity_ids: d.entity_ids,
        not_before_tai_ns: d.not_before_tai_ns,
        expires_tai_ns: d.expires_tai_ns,
        granted_by: d.granted_by,
        reason: d.reason,
    });
    Ok(DelegationTable::from_delegations(delegations))
}

// ---------------------------------------------------------------------------------------
// R3.1 (`docs/aiplane-plan.md` round 2's declared gap; `docs/open-questions.md` question
// 206's open item): a service role table, gating `Dispatch`/`Ack`/`Expire`/`Fail` on a
// verified OIDC **service** subject -- the identical `crate::oidc::verify` path `Authorize`'s
// `principal_token` already takes, never a second verifier. A service principal is
// distinguished from a human one structurally, not by any claim the secsso contract itself
// declares (there is no human-vs-service field on `Principal`, `authority.proto`'s own doc
// comment on that message explains why not): **a verified token is treated as a service
// principal exactly when at least one of its groups grants -- via this table -- the specific
// RPC being called.** A purely-human token (every one of its groups is a human `roles` entry,
// never a `service_roles` one) therefore always fails this table's own lookup and is refused
// [`ServiceAuthzError::ServiceRoleNotGranted`] -- the same refusal a service token naming no
// granting role at all gets, which is exactly the point: "service principal" means "holds a
// service role," not "isn't human" (a property this crate has no other way to observe), and
// [`check_service_roles_disjoint`] is what makes that distinction meaningful rather than
// accidental (see its own doc comment).
// ---------------------------------------------------------------------------------------

/// The four service RPCs a `service_roles` entry may grant (R3.1) -- `Dispatch`/`Ack`/
/// `Expire`/`Fail`, spelled in `profiles/execution.yaml` exactly as [`Self::name`] renders
/// them (lowercase, the RPC's own wire name). Not `Authorize`: that RPC's own gate is
/// [`authorize_command`]/[`RoleTable`] above, human-oriented (roles, MFA, delegations), and
/// deliberately untouched by this table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ServiceRpc {
    Dispatch,
    Ack,
    Expire,
    Fail,
}

impl ServiceRpc {
    /// The exact lowercase spelling `profiles/execution.yaml`'s `authority.service_roles`
    /// lists use, and the identical spelling this type's own `Display` below renders (so a
    /// counted refusal's `Debug`/`Display` text and the profile's own YAML never show two
    /// different spellings of the same RPC).
    pub fn name(self) -> &'static str {
        match self {
            ServiceRpc::Dispatch => "dispatch",
            ServiceRpc::Ack => "ack",
            ServiceRpc::Expire => "expire",
            ServiceRpc::Fail => "fail",
        }
    }

    /// The inverse of [`Self::name`] -- `None` for anything else, never a panic; a profile
    /// that misspells an entry is a [`ServiceRoleConfigError::UnknownRpc`], not a silent
    /// no-op.
    fn parse(s: &str) -> Option<Self> {
        match s {
            "dispatch" => Some(ServiceRpc::Dispatch),
            "ack" => Some(ServiceRpc::Ack),
            "expire" => Some(ServiceRpc::Expire),
            "fail" => Some(ServiceRpc::Fail),
            _ => None,
        }
    }
}

impl std::fmt::Display for ServiceRpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A profile's declared service-role table (`profiles/execution.yaml`'s `authority.
/// service_roles`, loaded by [`ServiceRoleTable::from_config`] and validated for disjointness
/// against the human [`RoleTable`] by [`check_service_roles_disjoint`] at
/// [`load_profile_authz_config`] time) -- the service-principal analogue of [`RoleTable`],
/// gating [`ServiceRpc`] values instead of command classes. Deny by default, structurally: an
/// empty table (no `service_roles:` block at all) makes [`Self::granting_role`] return `None`
/// for every group and every RPC, exactly [`RoleTable::granting_role`]'s own reasoning.
#[derive(Debug, Clone, Default)]
pub struct ServiceRoleTable {
    bindings: Vec<(String, Vec<ServiceRpc>)>,
}

/// The one way building a [`ServiceRoleTable`] from a profile's raw `BTreeMap<String,
/// Vec<String>>` can fail: an rpc name [`ServiceRpc::parse`] does not recognize.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ServiceRoleConfigError {
    #[error(
        "authority.service_roles role {role:?} lists unknown rpc {rpc:?}: expected one of \
         \"dispatch\", \"ack\", \"expire\", \"fail\""
    )]
    UnknownRpc { role: String, rpc: String },
}

impl ServiceRoleTable {
    /// Builds a table from the profile's raw `role -> [rpc name, ...]` map (the shape
    /// [`ProfileAuthzConfig::service_roles`] already deserializes into) -- `BTreeMap`, never
    /// `HashMap` (ADR-004), so iteration below is deterministic; the resulting `Vec` itself is
    /// consulted only by [`Self::role_grants`]'s linear `find`, which never depends on order
    /// for correctness (every entry's own `role` is compared, not "the first match by
    /// insertion order"), so the two are equivalent here to any deterministic order.
    pub fn from_config(service_roles: &BTreeMap<String, Vec<String>>) -> Result<Self, ServiceRoleConfigError> {
        let mut bindings = Vec::with_capacity(service_roles.len());
        for (role, rpcs) in service_roles {
            let mut parsed = Vec::with_capacity(rpcs.len());
            for rpc in rpcs {
                parsed.push(ServiceRpc::parse(rpc).ok_or_else(|| ServiceRoleConfigError::UnknownRpc { role: role.clone(), rpc: rpc.clone() })?);
            }
            bindings.push((role.clone(), parsed));
        }
        Ok(Self { bindings })
    }

    /// `true` only if `role` is present in this table *and* its granted rpc list contains
    /// `rpc` -- no wildcard here (unlike [`RoleTable::role_grants`]'s [`WILDCARD`]): R3.1's
    /// brief lists exactly four grantable RPCs and a role either lists the one being called
    /// or it does not; there is no "every RPC" shorthand to get wrong.
    fn role_grants(&self, role: &str, rpc: ServiceRpc) -> bool {
        self.bindings.iter().find(|(r, _)| r == role).is_some_and(|(_, rpcs)| rpcs.contains(&rpc))
    }

    /// The first of `groups` (claim order, per [`RoleTable::granting_role`]'s identical
    /// convention) that [`Self::role_grants`] `rpc`, if any.
    pub fn granting_role<'a>(&self, groups: &'a [String], rpc: ServiceRpc) -> Option<&'a str> {
        groups.iter().map(String::as_str).find(|g| self.role_grants(g, rpc))
    }
}

/// R3.1's own required check, run once at [`load_profile_authz_config`] time: **the keys of
/// `authority.service_roles` and `authority.roles` must never overlap.** Without this, a
/// human authorizer's own group (a role granting, say, `"mode"` under `authority.roles`)
/// could silently double as a `Dispatch`/`Ack`/`Expire`/`Fail` credential the moment the same
/// role name also appeared under `authority.service_roles` -- and a token minted for a person
/// (carrying that group because they hold a human command-authorization role) would then be
/// able to drive the simulated asset directly, a privilege escalation this crate has no other
/// structural way to prevent (there is no human-vs-service claim to check instead -- see this
/// module's own R3.1 section header comment). Checked with a typed error naming every
/// overlapping role, not merely the first, so a profile author sees the whole problem in one
/// error rather than fixing one name at a time.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OverlappingServiceRoleError {
    #[error(
        "authority.service_roles and authority.roles must be disjoint, but role(s) {overlapping:?} \
         appear in both -- a human authorizer's group must never silently double as a \
         dispatch/ack/expire/fail credential, and a token minted for a person must never be \
         able to drive the asset"
    )]
    Overlap { overlapping: Vec<String> },
}

fn check_service_roles_disjoint(roles: &BTreeMap<String, Vec<String>>, service_roles: &BTreeMap<String, Vec<String>>) -> Result<(), OverlappingServiceRoleError> {
    let overlapping: Vec<String> = service_roles.keys().filter(|k| roles.contains_key(*k)).cloned().collect();
    if overlapping.is_empty() {
        Ok(())
    } else {
        Err(OverlappingServiceRoleError::Overlap { overlapping })
    }
}

/// Every way [`authorize_service_call`] can refuse -- today, exactly one way, but its own
/// typed enum (not a bare `Result<_, String>`) matching this crate's rule against a generic
/// refusal, and so it implements [`Counted`] uniformly with every other refusal family here.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ServiceAuthzError {
    /// No group in `principal.groups` is a service role granting `rpc` -- this is the single
    /// refusal both "an unrecognized/unlisted service role" and "a purely human token" (whose
    /// groups are never a `service_roles` key at all, by [`check_service_roles_disjoint`]'s
    /// own construction) produce; see this module's own R3.1 section header comment for why
    /// that single refusal is exactly what "service principal" is defined to mean here.
    #[error(
        "service role gate refused -- none of groups {groups:?} is a service role granting rpc {rpc} \
         (crate::authz, deny-by-default: an unlisted role, a role that does not list this rpc, or a \
         purely human role -- disjoint from service_roles by construction -- is refused)"
    )]
    ServiceRoleNotGranted { groups: Vec<String>, rpc: ServiceRpc },
}

impl Counted for ServiceAuthzError {
    fn code(&self) -> &'static str {
        match self {
            ServiceAuthzError::ServiceRoleNotGranted { .. } => "service_role_not_granted",
        }
    }
}

/// The one entry point: does `principal` (already verified, exactly the same
/// [`crate::oidc::verify`] path `Authorize`'s own `principal_token` takes) hold a service role
/// granting `rpc`? Returns the granting role's own name on success (folded into
/// `CommandTransition.reason`, mirroring [`format_authz_reason`]'s own "name which role
/// granted it" convention) or the one typed [`ServiceAuthzError`] on refusal.
pub fn authorize_service_call(principal: &Principal, rpc: ServiceRpc, table: &ServiceRoleTable) -> Result<String, ServiceAuthzError> {
    table
        .granting_role(&principal.groups, rpc)
        .map(str::to_string)
        .ok_or_else(|| ServiceAuthzError::ServiceRoleNotGranted { groups: principal.groups.clone(), rpc })
}

/// The `authority:` block's A2.2 additions (`profiles/execution.yaml`, `profiles/README.md`)
/// -- parsed from the *same* YAML block `crate::policy::load_profile_authority_config` reads
/// its own A1.2 fields from (a second, independent `serde_yaml::from_str` over the identical
/// text, each pulling out only the keys it needs -- the same "deserialize just the block you
/// need" pattern that module's own doc already establishes; unknown keys are ignored by
/// `serde`'s default behaviour on both sides, so neither loader needs to know about the
/// other's fields).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ProfileAuthzConfig {
    /// Role -> the command classes it grants (see [`RoleTable::from_config`]). `BTreeMap`,
    /// never `HashMap` (ADR-004) -- a role table is never a source of non-deterministic
    /// iteration order.
    #[serde(default)]
    pub roles: BTreeMap<String, Vec<String>>,
    /// `amr` values that satisfy the MFA gate for a hazardous command class -- see the module
    /// doc's "MFA gate" section.
    #[serde(default)]
    pub mfa_amr_methods: Vec<String>,
    /// The `acr` value that satisfies the MFA gate, or empty for "not configured" -- see the
    /// module doc's "MFA gate" section for why this is exact-match only and why an empty
    /// string here can never match an empty `Principal.acr`.
    #[serde(default)]
    pub mfa_acr: String,
    /// Repo-relative path to the delegations YAML file [`load_delegations_file`] reads, or
    /// empty for "no delegations file configured" (an empty [`DelegationTable`], not an
    /// error).
    #[serde(default)]
    pub delegations_path: String,
    /// R3.1: role -> the [`ServiceRpc`] names it grants (`"dispatch"`/`"ack"`/`"expire"`/
    /// `"fail"`), see [`ServiceRoleTable::from_config`]. Deny by default: a role absent here
    /// grants nothing. Checked disjoint from [`Self::roles`] above at
    /// [`load_profile_authz_config`] time -- see [`check_service_roles_disjoint`]'s own doc
    /// for why.
    #[serde(default)]
    pub service_roles: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ProfileDocument {
    authority: ProfileAuthzConfig,
}

/// Every way [`load_profile_authz_config`] can fail: the YAML itself does not parse, its
/// `service_roles` block names an unrecognized rpc, or `service_roles`/`roles` are not
/// disjoint (R3.1). Distinct from the plain `serde_yaml::Error` this function returned before
/// R3.1 added the two new checks -- both are load-time refusals a profile author needs to see
/// named, not folded into one generic parse failure.
#[derive(Debug, Error)]
pub enum LoadProfileAuthzConfigError {
    #[error("parsing authority: block: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error(transparent)]
    OverlappingServiceRole(#[from] OverlappingServiceRoleError),
    #[error(transparent)]
    ServiceRoleConfig(#[from] ServiceRoleConfigError),
}

/// Reads the A2.2/R3.1 fields of the `authority:` block out of a full profile YAML document's
/// text -- see [`ProfileAuthzConfig`]'s own doc for why this is a second, independent parse of
/// the same text `crate::policy::load_profile_authority_config` already reads, not a change
/// to that function's own return type. R3.1 additions, both checked here so a bad profile is
/// refused at load time rather than at the first `Dispatch`/`Ack`/`Expire`/`Fail` call that
/// happens to exercise it: `service_roles` and `roles` must be disjoint
/// ([`check_service_roles_disjoint`]), and every rpc name `service_roles` lists must parse
/// ([`ServiceRoleTable::from_config`], whose only job here is validation -- the caller builds
/// its own runtime [`ServiceRoleTable`] separately, exactly the existing convention
/// `crate::policy::load_profile_authority_config`'s own sibling loaders already follow of
/// validating shape here and constructing the "real" runtime value at the call site).
pub fn load_profile_authz_config(yaml: &str) -> Result<ProfileAuthzConfig, LoadProfileAuthzConfigError> {
    let config = serde_yaml::from_str::<ProfileDocument>(yaml)?.authority;
    check_service_roles_disjoint(&config.roles, &config.service_roles)?;
    ServiceRoleTable::from_config(&config.service_roles)?;
    Ok(config)
}

/// Which of `authority.delegations_path`'s two states applied: relative paths are resolved
/// against `repo_root`. Returns an empty table (never an error) when `delegations_path` is
/// empty -- "no delegations file configured" is a valid, deliberate profile shape (see
/// [`ProfileAuthzConfig::delegations_path`]'s own doc).
pub fn load_delegations(repo_root: &Path, delegations_path: &str) -> std::io::Result<DelegationTable> {
    if delegations_path.is_empty() {
        return Ok(DelegationTable::default());
    }
    let text = std::fs::read_to_string(repo_root.join(delegations_path))?;
    load_delegations_file(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

/// Every way [`authorize_command`] can refuse -- see the module doc for the full contract.
/// Every variant names exactly one failed check; there is no `AuthzError::NotAuthorized`
/// catch-all anywhere in this enum, matching this crate's rule against a generic refusal
/// (`crates/av-command/src/oidc.rs`'s own "refusal vocabulary" section states the identical
/// rule for `TokenError`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AuthzError {
    /// No group in `principal.groups` is a role granting `command_class` (`delegation_id` was
    /// empty, so the role table -- not a delegation -- was consulted). See the module doc's
    /// "Deny by default" section, point 1.
    #[error(
        "authorize: role gate refused -- none of groups {groups:?} is a role granting command_class {command_class:?} \
         (crate::authz, deny-by-default: an unlisted role, or a role that does not list this class, is refused)"
    )]
    RoleNotGranted { groups: Vec<String>, command_class: String },

    /// `delegation_id` named no [`Delegation`] in the configured table.
    #[error("authorize: delegation {delegation_id:?} not found")]
    DelegationNotFound { delegation_id: String },

    /// The delegation exists but was granted to a different subject.
    #[error("authorize: delegation {delegation_id:?} was granted to subject {expected_subject:?}, not {actual_subject:?}")]
    DelegationSubjectMismatch { delegation_id: String, expected_subject: String, actual_subject: String },

    /// The delegation's subject matches, but its `command_classes` does not cover this
    /// command's class.
    #[error("authorize: delegation {delegation_id:?} does not cover command_class {command_class:?} (covers {covered:?})")]
    DelegationClassNotCovered { delegation_id: String, command_class: String, covered: Vec<String> },

    /// The delegation's subject and class match, but its `entity_ids` does not cover this
    /// command's entity.
    #[error("authorize: delegation {delegation_id:?} does not cover entity_id {entity_id:?} (covers {covered:?})")]
    DelegationEntityNotCovered { delegation_id: String, entity_id: String, covered: Vec<String> },

    /// `now_tai_ns < delegation.not_before_tai_ns`.
    #[error("authorize: delegation {delegation_id:?} not yet valid -- not_before_tai_ns={not_before_tai_ns} is after now_tai_ns={now_tai_ns}")]
    DelegationNotYetValid { delegation_id: String, not_before_tai_ns: i64, now_tai_ns: i64 },

    /// `now_tai_ns >= delegation.expires_tai_ns` -- refused **at the boundary second**,
    /// matching `crate::oidc::TokenError::Expired`'s identical comparison. See the module
    /// doc's "Deny by default" section, point 2.
    #[error("authorize: delegation {delegation_id:?} expired -- expires_tai_ns={expires_tai_ns} is at or before now_tai_ns={now_tai_ns}")]
    DelegationExpired { delegation_id: String, expires_tai_ns: i64, now_tai_ns: i64 },

    /// `Command.hazardous` is set and neither the `amr` nor the `acr` check passed. Its own,
    /// distinct message from [`Self::RoleNotGranted`] -- see the module doc's "MFA gate"
    /// section and point 3 of "Deny by default".
    #[error(
        "authorize: MFA gate refused -- command_class {command_class:?} is hazardous and requires one of amr {required_amr:?} \
         or acr {required_acr:?}; principal amr was {actual_amr:?} and acr was {actual_acr:?}"
    )]
    MfaRequired { command_class: String, required_amr: Vec<String>, required_acr: String, actual_amr: Vec<String>, actual_acr: String },
}

/// R3.1: `Authorize`'s role/MFA/delegation refusals now count too, since [`crate::counters::
/// Counters`] is available to this crate and ADR-004's "everything rejected is counted" rule
/// is unconditional -- not merely something the four service RPCs this round adds happen to
/// need.
impl Counted for AuthzError {
    fn code(&self) -> &'static str {
        match self {
            AuthzError::RoleNotGranted { .. } => "authz_role_not_granted",
            AuthzError::DelegationNotFound { .. } => "authz_delegation_not_found",
            AuthzError::DelegationSubjectMismatch { .. } => "authz_delegation_subject_mismatch",
            AuthzError::DelegationClassNotCovered { .. } => "authz_delegation_class_not_covered",
            AuthzError::DelegationEntityNotCovered { .. } => "authz_delegation_entity_not_covered",
            AuthzError::DelegationNotYetValid { .. } => "authz_delegation_not_yet_valid",
            AuthzError::DelegationExpired { .. } => "authz_delegation_expired",
            AuthzError::MfaRequired { .. } => "authz_mfa_required",
        }
    }
}

/// How [`authorize_command`] granted its decision -- carried into `CommandTransition.reason`
/// (via [`format_authz_reason`]) and `CommandTransition.delegation_id` (already carried by
/// `crate::state::authorize`'s existing signature -- this module supplies its value, never a
/// second field for it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizedVia {
    Role { role: String },
    Delegation { delegation_id: String },
}

/// The MFA outcome for a granted decision -- always [`Self::NotRequired`] for a non-hazardous
/// command class; see the module doc's "MFA gate" section for the other two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MfaOutcome {
    NotRequired,
    VerifiedAmr(String),
    VerifiedAcr(String),
}

/// The full result of a granted [`authorize_command`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthzDecision {
    pub via: AuthorizedVia,
    pub mfa: MfaOutcome,
}

/// Groups [`authorize_command`]'s role/delegation/MFA configuration -- exactly the way
/// `crate::ledger::CommandMeta` groups `Ledger::append`'s own arguments and
/// `crate::service::AuthzConfig` groups `CommandAuthorityServiceImpl::new`'s -- so that
/// function gains A2.2's configuration without tripping clippy's `too_many_arguments` lint;
/// this crate's rule against a lint-suppressing attribute on hand-written items means the fix is grouping the
/// arguments, never silencing the lint in place.
#[derive(Debug, Clone, Copy)]
pub struct AuthzContext<'a> {
    pub role_table: &'a RoleTable,
    pub delegations: &'a DelegationTable,
    pub mfa_amr_methods: &'a [String],
    pub mfa_acr: &'a str,
}

/// The one entry point: does `principal` (already verified by A2.1's `crate::oidc::verify`)
/// have authority to authorize `command`, either via its own role or via `delegation_id`
/// (empty for "no delegation claimed"), and -- if `command.hazardous` -- does it also satisfy
/// the MFA gate? See the module doc for the full contract, the exact check order, and why
/// each refusal is its own typed variant.
pub fn authorize_command(principal: &Principal, command: &Command, delegation_id: &str, ctx: AuthzContext<'_>, now_tai_ns: i64) -> Result<AuthzDecision, AuthzError> {
    let via = if delegation_id.is_empty() {
        let role = ctx
            .role_table
            .granting_role(&principal.groups, &command.command_class)
            .ok_or_else(|| AuthzError::RoleNotGranted { groups: principal.groups.clone(), command_class: command.command_class.clone() })?;
        AuthorizedVia::Role { role: role.to_string() }
    } else {
        let delegation = ctx.delegations.get(delegation_id).ok_or_else(|| AuthzError::DelegationNotFound { delegation_id: delegation_id.to_string() })?;

        if delegation.subject != principal.sub {
            return Err(AuthzError::DelegationSubjectMismatch {
                delegation_id: delegation_id.to_string(),
                expected_subject: delegation.subject.clone(),
                actual_subject: principal.sub.clone(),
            });
        }
        if !delegation.command_classes.iter().any(|c| c == &command.command_class || c == WILDCARD) {
            return Err(AuthzError::DelegationClassNotCovered {
                delegation_id: delegation_id.to_string(),
                command_class: command.command_class.clone(),
                covered: delegation.command_classes.clone(),
            });
        }
        if !delegation.entity_ids.iter().any(|e| e == &command.entity_id || e == WILDCARD) {
            return Err(AuthzError::DelegationEntityNotCovered {
                delegation_id: delegation_id.to_string(),
                entity_id: command.entity_id.clone(),
                covered: delegation.entity_ids.clone(),
            });
        }
        if now_tai_ns < delegation.not_before_tai_ns {
            return Err(AuthzError::DelegationNotYetValid {
                delegation_id: delegation_id.to_string(),
                not_before_tai_ns: delegation.not_before_tai_ns,
                now_tai_ns,
            });
        }
        if now_tai_ns >= delegation.expires_tai_ns {
            return Err(AuthzError::DelegationExpired { delegation_id: delegation_id.to_string(), expires_tai_ns: delegation.expires_tai_ns, now_tai_ns });
        }
        AuthorizedVia::Delegation { delegation_id: delegation_id.to_string() }
    };

    let mfa = if command.hazardous {
        if let Some(method) = ctx.mfa_amr_methods.iter().find(|m| principal.amr.contains(m)) {
            MfaOutcome::VerifiedAmr(method.clone())
        } else if !ctx.mfa_acr.is_empty() && principal.acr == ctx.mfa_acr {
            MfaOutcome::VerifiedAcr(principal.acr.clone())
        } else {
            return Err(AuthzError::MfaRequired {
                command_class: command.command_class.clone(),
                required_amr: ctx.mfa_amr_methods.to_vec(),
                required_acr: ctx.mfa_acr.to_string(),
                actual_amr: principal.amr.clone(),
                actual_acr: principal.acr.clone(),
            });
        }
    } else {
        MfaOutcome::NotRequired
    };

    Ok(AuthzDecision { via, mfa })
}

/// The `CommandTransition.reason` text for a granted [`authorize_command`] decision -- the
/// audit-and-ledger-visible record of *which* role or delegation granted it and *how* MFA was
/// satisfied (or that it was not required). Mirrors `crate::authority::format_reason`'s own
/// key=value-token grammar, adapted for this decision's own fields.
pub fn format_authz_reason(command_class: &str, decision: &AuthzDecision) -> String {
    let via = match &decision.via {
        AuthorizedVia::Role { role } => format!("role={role:?}"),
        AuthorizedVia::Delegation { delegation_id } => format!("delegation={delegation_id:?}"),
    };
    let mfa = match &decision.mfa {
        MfaOutcome::NotRequired => "not_required".to_string(),
        MfaOutcome::VerifiedAmr(method) => format!("amr={method:?}"),
        MfaOutcome::VerifiedAcr(value) => format!("acr={value:?}"),
    };
    format!("authorize: command_class={command_class:?} via={via} mfa={mfa} (crate::authz)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(sub: &str, groups: &[&str], amr: &[&str], acr: &str) -> Principal {
        Principal {
            sub: sub.to_string(),
            groups: groups.iter().map(|s| s.to_string()).collect(),
            amr: amr.iter().map(|s| s.to_string()).collect(),
            acr: acr.to_string(),
            issuer: "https://sso.test.example/".to_string(),
            audience: "av-command".to_string(),
            jti: "jti-1".to_string(),
            issued_at_tai_ns: 0,
            expiry_tai_ns: i64::MAX,
        }
    }

    fn command(class: &str, entity_id: &str, hazardous: bool) -> Command {
        Command { command_class: class.to_string(), entity_id: entity_id.to_string(), hazardous, ..Command::default() }
    }

    fn role_table() -> RoleTable {
        let mut roles = BTreeMap::new();
        roles.insert("operators".to_string(), vec!["mode".to_string()]);
        roles.insert("burn-authorizers".to_string(), vec!["mode".to_string(), "burn".to_string()]);
        roles.insert("safety-officers".to_string(), vec![WILDCARD.to_string()]);
        RoleTable::from_config(&roles)
    }

    #[test]
    fn the_right_role_authorizes() {
        let table = role_table();
        let p = principal("operator-1", &["operators"], &[], "");
        let cmd = command("mode", "sat-1", false);
        let decision = authorize_command(&p, &cmd, "", AuthzContext { role_table: &table, delegations: &DelegationTable::default(), mfa_amr_methods: &[], mfa_acr: "" }, 0).expect("operators grants mode");
        assert_eq!(decision.via, AuthorizedVia::Role { role: "operators".to_string() });
        assert_eq!(decision.mfa, MfaOutcome::NotRequired);
    }

    #[test]
    fn a_wildcard_role_grants_every_class() {
        let table = role_table();
        let p = principal("safety-1", &["safety-officers"], &[], "");
        for class in ["mode", "burn", "payload", "anything"] {
            let cmd = command(class, "sat-1", false);
            authorize_command(&p, &cmd, "", AuthzContext { role_table: &table, delegations: &DelegationTable::default(), mfa_amr_methods: &[], mfa_acr: "" }, 0).unwrap_or_else(|e| panic!("{class}: {e}"));
        }
    }

    #[test]
    fn the_wrong_role_is_refused_with_the_exact_reason() {
        let table = role_table();
        let p = principal("viewer-1", &["viewers"], &[], "");
        let cmd = command("mode", "sat-1", false);
        let err = authorize_command(&p, &cmd, "", AuthzContext { role_table: &table, delegations: &DelegationTable::default(), mfa_amr_methods: &[], mfa_acr: "" }, 0).unwrap_err();
        assert_eq!(
            err.to_string(),
            "authorize: role gate refused -- none of groups [\"viewers\"] is a role granting command_class \"mode\" \
             (crate::authz, deny-by-default: an unlisted role, or a role that does not list this class, is refused)"
        );
    }

    #[test]
    fn a_role_present_in_the_table_but_not_listing_the_class_is_refused() {
        let table = role_table();
        let p = principal("op-1", &["operators"], &[], "");
        let cmd = command("burn", "sat-1", false); // operators grants only "mode"
        let err = authorize_command(&p, &cmd, "", AuthzContext { role_table: &table, delegations: &DelegationTable::default(), mfa_amr_methods: &[], mfa_acr: "" }, 0).unwrap_err();
        assert!(matches!(err, AuthzError::RoleNotGranted { .. }), "{err:?}");
    }

    #[test]
    fn an_empty_role_table_denies_every_class_never_allows() {
        let table = RoleTable::default();
        let p = principal("op-1", &["operators"], &[], "");
        let cmd = command("mode", "sat-1", false);
        let err = authorize_command(&p, &cmd, "", AuthzContext { role_table: &table, delegations: &DelegationTable::default(), mfa_amr_methods: &[], mfa_acr: "" }, 0).unwrap_err();
        assert!(matches!(err, AuthzError::RoleNotGranted { .. }), "an empty/no table must never mean allow: {err:?}");
    }

    #[test]
    fn hazardous_without_mfa_is_refused_with_a_distinct_reason_from_wrong_role() {
        let table = role_table();
        let p = principal("op-1", &["operators"], &[], ""); // right role, no amr/acr at all
        let cmd = command("mode", "sat-1", true);
        let err = authorize_command(&p, &cmd, "", AuthzContext { role_table: &table, delegations: &DelegationTable::default(), mfa_amr_methods: &["otp".to_string()], mfa_acr: "" }, 0).unwrap_err();
        assert_eq!(
            err.to_string(),
            "authorize: MFA gate refused -- command_class \"mode\" is hazardous and requires one of amr [\"otp\"] or acr \"\"; \
             principal amr was [] and acr was \"\""
        );
        let wrong_role_err = authorize_command(&principal("v", &["viewers"], &[], ""), &cmd, "", AuthzContext { role_table: &table, delegations: &DelegationTable::default(), mfa_amr_methods: &["otp".to_string()], mfa_acr: "" }, 0).unwrap_err();
        assert_ne!(err.to_string(), wrong_role_err.to_string(), "MFA and role refusals must read differently");
    }

    /// **Defect this task's own brief names first**: an absent `amr` claim must never
    /// accidentally satisfy the MFA gate.
    #[test]
    fn hazardous_with_an_empty_amr_and_no_configured_acr_is_refused() {
        let table = role_table();
        let p = principal("op-1", &["operators"], &[], "");
        let cmd = command("mode", "sat-1", true);
        let err = authorize_command(&p, &cmd, "", AuthzContext { role_table: &table, delegations: &DelegationTable::default(), mfa_amr_methods: &[], mfa_acr: "" }, 0).unwrap_err();
        assert!(matches!(err, AuthzError::MfaRequired { .. }), "{err:?}");
    }

    #[test]
    fn hazardous_with_a_matching_amr_method_succeeds() {
        let table = role_table();
        let p = principal("op-1", &["operators"], &["pwd", "otp"], "");
        let cmd = command("mode", "sat-1", true);
        let decision = authorize_command(&p, &cmd, "", AuthzContext { role_table: &table, delegations: &DelegationTable::default(), mfa_amr_methods: &["otp".to_string()], mfa_acr: "" }, 0).unwrap();
        assert_eq!(decision.mfa, MfaOutcome::VerifiedAmr("otp".to_string()));
    }

    #[test]
    fn hazardous_with_a_matching_acr_succeeds_when_amr_does_not_match() {
        let table = role_table();
        let p = principal("op-1", &["operators"], &["pwd"], "urn:mfa:otp");
        let cmd = command("mode", "sat-1", true);
        let decision = authorize_command(&p, &cmd, "", AuthzContext { role_table: &table, delegations: &DelegationTable::default(), mfa_amr_methods: &["webauthn".to_string()], mfa_acr: "urn:mfa:otp" }, 0).unwrap();
        assert_eq!(decision.mfa, MfaOutcome::VerifiedAcr("urn:mfa:otp".to_string()));
    }

    /// An empty configured `mfa_acr` must never match an empty `principal.acr` -- both being
    /// "not configured"/"not present" must not read as a match.
    #[test]
    fn an_empty_configured_acr_never_matches_an_empty_principal_acr() {
        let table = role_table();
        let p = principal("op-1", &["operators"], &[], "");
        let cmd = command("mode", "sat-1", true);
        let err = authorize_command(&p, &cmd, "", AuthzContext { role_table: &table, delegations: &DelegationTable::default(), mfa_amr_methods: &[], mfa_acr: "" }, 0).unwrap_err();
        assert!(matches!(err, AuthzError::MfaRequired { .. }));
    }

    fn delegation(id: &str, subject: &str, classes: &[&str], entities: &[&str], not_before: i64, expires: i64) -> Delegation {
        Delegation {
            id: id.to_string(),
            subject: subject.to_string(),
            command_classes: classes.iter().map(|s| s.to_string()).collect(),
            entity_ids: entities.iter().map(|s| s.to_string()).collect(),
            not_before_tai_ns: not_before,
            expires_tai_ns: expires,
            granted_by: "ops-lead".to_string(),
            reason: "test delegation".to_string(),
        }
    }

    fn delegation_table(delegations: Vec<Delegation>) -> DelegationTable {
        DelegationTable::from_delegations(delegations)
    }

    /// **A delegation grants a class the role does not** -- this task's own sixth test.
    #[test]
    fn a_delegation_grants_a_class_the_role_does_not() {
        let table = role_table(); // "operators" grants only "mode"
        let delegations = delegation_table(vec![delegation("delegation-1", "operator-1", &["burn"], &["sat-1"], 0, 10_000)]);
        let p = principal("operator-1", &["operators"], &[], "");
        let cmd = command("burn", "sat-1", false);

        // Without the delegation, the role alone refuses.
        let err = authorize_command(&p, &cmd, "", AuthzContext { role_table: &table, delegations: &delegations, mfa_amr_methods: &[], mfa_acr: "" }, 5_000).unwrap_err();
        assert!(matches!(err, AuthzError::RoleNotGranted { .. }));

        // With the delegation named, it is granted.
        let decision = authorize_command(&p, &cmd, "delegation-1", AuthzContext { role_table: &table, delegations: &delegations, mfa_amr_methods: &[], mfa_acr: "" }, 5_000).unwrap();
        assert_eq!(decision.via, AuthorizedVia::Delegation { delegation_id: "delegation-1".to_string() });
    }

    #[test]
    fn a_delegation_for_a_different_subject_is_refused() {
        let delegations = delegation_table(vec![delegation("delegation-1", "someone-else", &["burn"], &["sat-1"], 0, 10_000)]);
        let p = principal("operator-1", &[], &[], "");
        let cmd = command("burn", "sat-1", false);
        let err = authorize_command(&p, &cmd, "delegation-1", AuthzContext { role_table: &RoleTable::default(), delegations: &delegations, mfa_amr_methods: &[], mfa_acr: "" }, 5_000).unwrap_err();
        assert!(matches!(err, AuthzError::DelegationSubjectMismatch { .. }), "{err:?}");
    }

    #[test]
    fn a_delegation_not_covering_the_command_class_is_refused() {
        let delegations = delegation_table(vec![delegation("delegation-1", "operator-1", &["mode"], &["sat-1"], 0, 10_000)]);
        let p = principal("operator-1", &[], &[], "");
        let cmd = command("burn", "sat-1", false);
        let err = authorize_command(&p, &cmd, "delegation-1", AuthzContext { role_table: &RoleTable::default(), delegations: &delegations, mfa_amr_methods: &[], mfa_acr: "" }, 5_000).unwrap_err();
        assert!(matches!(err, AuthzError::DelegationClassNotCovered { .. }), "{err:?}");
    }

    #[test]
    fn a_delegation_not_covering_the_entity_is_refused() {
        let delegations = delegation_table(vec![delegation("delegation-1", "operator-1", &["burn"], &["sat-other"], 0, 10_000)]);
        let p = principal("operator-1", &[], &[], "");
        let cmd = command("burn", "sat-1", false);
        let err = authorize_command(&p, &cmd, "delegation-1", AuthzContext { role_table: &RoleTable::default(), delegations: &delegations, mfa_amr_methods: &[], mfa_acr: "" }, 5_000).unwrap_err();
        assert!(matches!(err, AuthzError::DelegationEntityNotCovered { .. }), "{err:?}");
    }

    #[test]
    fn an_unknown_delegation_id_is_refused_not_found() {
        let p = principal("operator-1", &[], &[], "");
        let cmd = command("burn", "sat-1", false);
        let err = authorize_command(&p, &cmd, "no-such-delegation", AuthzContext { role_table: &RoleTable::default(), delegations: &DelegationTable::default(), mfa_amr_methods: &[], mfa_acr: "" }, 5_000).unwrap_err();
        assert_eq!(err.to_string(), "authorize: delegation \"no-such-delegation\" not found");
    }

    /// **The expiry boundary this task pins**: `now_tai_ns == expires_tai_ns` is refused;
    /// `expires_tai_ns - 1` is not.
    #[test]
    fn delegation_expiry_is_refused_at_the_boundary_second_one_nanosecond_earlier_is_not() {
        let delegations = delegation_table(vec![delegation("delegation-1", "operator-1", &["burn"], &["sat-1"], 0, 10_000)]);
        let p = principal("operator-1", &[], &[], "");
        let cmd = command("burn", "sat-1", false);

        let err = authorize_command(&p, &cmd, "delegation-1", AuthzContext { role_table: &RoleTable::default(), delegations: &delegations, mfa_amr_methods: &[], mfa_acr: "" }, 10_000).unwrap_err();
        match err {
            AuthzError::DelegationExpired { expires_tai_ns, now_tai_ns, .. } => {
                assert_eq!(expires_tai_ns, 10_000);
                assert_eq!(now_tai_ns, 10_000);
            }
            other => panic!("expected DelegationExpired at the boundary, got {other:?}"),
        }

        authorize_command(&p, &cmd, "delegation-1", AuthzContext { role_table: &RoleTable::default(), delegations: &delegations, mfa_amr_methods: &[], mfa_acr: "" }, 9_999).expect("one nanosecond before expiry must not be refused");
    }

    #[test]
    fn delegation_not_before_boundary_is_valid_one_nanosecond_earlier_is_not() {
        let delegations = delegation_table(vec![delegation("delegation-1", "operator-1", &["burn"], &["sat-1"], 5_000, 10_000)]);
        let p = principal("operator-1", &[], &[], "");
        let cmd = command("burn", "sat-1", false);

        authorize_command(&p, &cmd, "delegation-1", AuthzContext { role_table: &RoleTable::default(), delegations: &delegations, mfa_amr_methods: &[], mfa_acr: "" }, 5_000).expect("at not_before exactly, the delegation is already valid");

        let err = authorize_command(&p, &cmd, "delegation-1", AuthzContext { role_table: &RoleTable::default(), delegations: &delegations, mfa_amr_methods: &[], mfa_acr: "" }, 4_999).unwrap_err();
        assert!(matches!(err, AuthzError::DelegationNotYetValid { .. }), "{err:?}");
    }

    #[test]
    fn a_delegation_covers_every_class_and_entity_via_the_wildcard() {
        let delegations = delegation_table(vec![delegation("delegation-1", "operator-1", &[WILDCARD], &[WILDCARD], 0, 10_000)]);
        let p = principal("operator-1", &[], &[], "");
        for (class, entity) in [("burn", "sat-1"), ("payload", "sat-9")] {
            let cmd = command(class, entity, false);
            authorize_command(&p, &cmd, "delegation-1", AuthzContext { role_table: &RoleTable::default(), delegations: &delegations, mfa_amr_methods: &[], mfa_acr: "" }, 5_000).unwrap_or_else(|e| panic!("{class}/{entity}: {e}"));
        }
    }

    #[test]
    fn load_profile_authz_config_reads_the_authority_blocks_a2_2_fields() {
        let yaml = r#"
authority:
  policy_dir: profiles/policies/authority
  allow_entrypoint: data.altavista.authority.allow
  deny_entrypoint: data.altavista.authority.deny
  rate_window_ns: 3600000000000
  roles:
    operators: ["mode"]
    burn-authorizers: ["mode", "burn"]
  mfa_amr_methods: ["otp", "webauthn"]
  mfa_acr: "urn:mfa:otp"
  delegations_path: profiles/policies/authority/delegations.yaml
"#;
        let config = load_profile_authz_config(yaml).unwrap();
        assert_eq!(config.roles.get("operators"), Some(&vec!["mode".to_string()]));
        assert_eq!(config.roles.get("burn-authorizers"), Some(&vec!["mode".to_string(), "burn".to_string()]));
        assert_eq!(config.mfa_amr_methods, vec!["otp".to_string(), "webauthn".to_string()]);
        assert_eq!(config.mfa_acr, "urn:mfa:otp");
        assert_eq!(config.delegations_path, "profiles/policies/authority/delegations.yaml");
    }

    #[test]
    fn load_delegations_file_parses_a_real_document_and_an_empty_one() {
        let yaml = r#"
delegations:
  - id: delegation-1
    subject: operator-1
    command_classes: ["burn"]
    entity_ids: ["*"]
    not_before_tai_ns: 0
    expires_tai_ns: 10000
    granted_by: ops-lead
    reason: contingency
"#;
        let table = load_delegations_file(yaml).unwrap();
        let d = table.get("delegation-1").expect("delegation-1 present");
        assert_eq!(d.subject, "operator-1");
        assert_eq!(d.command_classes, vec!["burn".to_string()]);
        assert_eq!(d.entity_ids, vec![WILDCARD.to_string()]);

        let empty = load_delegations_file("delegations: []\n").unwrap();
        assert_eq!(empty.all().count(), 0);

        let absent_key = load_delegations_file("{}\n").unwrap();
        assert_eq!(absent_key.all().count(), 0, "a missing delegations: key parses as an empty table, not an error");
    }

    #[test]
    fn format_authz_reason_names_the_role_and_mfa_outcome() {
        let decision = AuthzDecision { via: AuthorizedVia::Role { role: "operators".to_string() }, mfa: MfaOutcome::NotRequired };
        assert_eq!(format_authz_reason("mode", &decision), "authorize: command_class=\"mode\" via=role=\"operators\" mfa=not_required (crate::authz)");

        let decision = AuthzDecision { via: AuthorizedVia::Delegation { delegation_id: "delegation-1".to_string() }, mfa: MfaOutcome::VerifiedAmr("otp".to_string()) };
        assert_eq!(format_authz_reason("burn", &decision), "authorize: command_class=\"burn\" via=delegation=\"delegation-1\" mfa=amr=\"otp\" (crate::authz)");
    }

    // ---------------------------------------------------------------------------------------
    // R3.1: ServiceRoleTable, the disjointness check, and authorize_service_call.
    // ---------------------------------------------------------------------------------------

    fn service_roles() -> BTreeMap<String, Vec<String>> {
        let mut m = BTreeMap::new();
        m.insert("dispatchers".to_string(), vec!["dispatch".to_string(), "ack".to_string(), "expire".to_string(), "fail".to_string()]);
        m.insert("ackers-only".to_string(), vec!["ack".to_string()]);
        m
    }

    #[test]
    fn service_role_table_grants_only_the_rpcs_it_lists() {
        let table = ServiceRoleTable::from_config(&service_roles()).unwrap();
        let groups = vec!["ackers-only".to_string()];
        assert_eq!(table.granting_role(&groups, ServiceRpc::Ack), Some("ackers-only"));
        assert_eq!(table.granting_role(&groups, ServiceRpc::Dispatch), None, "ackers-only lists only ack");
        assert_eq!(table.granting_role(&groups, ServiceRpc::Expire), None);
        assert_eq!(table.granting_role(&groups, ServiceRpc::Fail), None);
    }

    #[test]
    fn service_role_table_grants_every_rpc_it_lists() {
        let table = ServiceRoleTable::from_config(&service_roles()).unwrap();
        let groups = vec!["dispatchers".to_string()];
        for rpc in [ServiceRpc::Dispatch, ServiceRpc::Ack, ServiceRpc::Expire, ServiceRpc::Fail] {
            assert_eq!(table.granting_role(&groups, rpc), Some("dispatchers"), "{rpc}");
        }
    }

    /// Deny by default: an empty table (no `service_roles:` block at all) grants nothing to
    /// any group, for any rpc -- there is no third state that means "allow".
    #[test]
    fn an_empty_service_role_table_grants_nothing() {
        let table = ServiceRoleTable::default();
        let groups = vec!["dispatchers".to_string(), "anything".to_string()];
        for rpc in [ServiceRpc::Dispatch, ServiceRpc::Ack, ServiceRpc::Expire, ServiceRpc::Fail] {
            assert_eq!(table.granting_role(&groups, rpc), None, "{rpc}");
        }
    }

    /// A role present in the table but not listing the rpc being asked about grants nothing
    /// for that rpc -- "role known, rpc unlisted" is never a third state that means "allow".
    #[test]
    fn a_service_role_present_but_not_listing_the_rpc_grants_nothing_for_it() {
        let table = ServiceRoleTable::from_config(&service_roles()).unwrap();
        assert_eq!(table.granting_role(&["ackers-only".to_string()], ServiceRpc::Dispatch), None);
    }

    #[test]
    fn service_role_table_from_config_refuses_an_unknown_rpc_name() {
        let mut roles = BTreeMap::new();
        roles.insert("dispatchers".to_string(), vec!["dispatch".to_string(), "reboot".to_string()]);
        let err = ServiceRoleTable::from_config(&roles).unwrap_err();
        assert_eq!(err, ServiceRoleConfigError::UnknownRpc { role: "dispatchers".to_string(), rpc: "reboot".to_string() });
    }

    #[test]
    fn authorize_service_call_grants_the_role_that_lists_the_rpc() {
        let table = ServiceRoleTable::from_config(&service_roles()).unwrap();
        let p = principal("kernel-binding-1", &["dispatchers"], &[], "");
        let role = authorize_service_call(&p, ServiceRpc::Dispatch, &table).expect("dispatchers grants dispatch");
        assert_eq!(role, "dispatchers");
    }

    /// **This is the test that proves "service principal" means something**: a verified
    /// token whose groups grant a *human* command class (via `authority.roles`, exercised
    /// here through the exact same `role_table()` fixture this file's A2.2 tests already use)
    /// but no service role at all is refused on every one of the four service RPCs -- the
    /// identical [`ServiceAuthzError::ServiceRoleNotGranted`] refusal an unrecognized role
    /// gets, which is the point (see this module's own R3.1 section header comment).
    #[test]
    fn a_human_token_with_no_service_role_is_refused_on_every_service_rpc() {
        let service_table = ServiceRoleTable::from_config(&service_roles()).unwrap();
        // "operators" is a real, granting HUMAN role (role_table(), this file's own A2.2
        // fixture) but is not a key of service_roles() at all.
        let human = principal("operator-1", &["operators"], &[], "");
        for rpc in [ServiceRpc::Dispatch, ServiceRpc::Ack, ServiceRpc::Expire, ServiceRpc::Fail] {
            let err = authorize_service_call(&human, rpc, &service_table).unwrap_err();
            assert_eq!(err, ServiceAuthzError::ServiceRoleNotGranted { groups: vec!["operators".to_string()], rpc }, "{rpc}");
        }
    }

    #[test]
    fn authorize_service_call_refuses_a_role_that_does_not_list_this_rpc() {
        let table = ServiceRoleTable::from_config(&service_roles()).unwrap();
        let p = principal("kernel-binding-1", &["ackers-only"], &[], "");
        let err = authorize_service_call(&p, ServiceRpc::Dispatch, &table).unwrap_err();
        assert_eq!(err, ServiceAuthzError::ServiceRoleNotGranted { groups: vec!["ackers-only".to_string()], rpc: ServiceRpc::Dispatch });
    }

    #[test]
    fn service_authz_error_counted_codes_are_stable_snake_case() {
        let err = ServiceAuthzError::ServiceRoleNotGranted { groups: vec![], rpc: ServiceRpc::Dispatch };
        assert_eq!(err.code(), "service_role_not_granted");
    }

    #[test]
    fn authz_error_counted_codes_are_stable_snake_case_and_distinct() {
        let codes: Vec<&'static str> = vec![
            AuthzError::RoleNotGranted { groups: vec![], command_class: "x".to_string() }.code(),
            AuthzError::DelegationNotFound { delegation_id: "x".to_string() }.code(),
            AuthzError::DelegationSubjectMismatch { delegation_id: "x".to_string(), expected_subject: "a".to_string(), actual_subject: "b".to_string() }.code(),
            AuthzError::DelegationClassNotCovered { delegation_id: "x".to_string(), command_class: "x".to_string(), covered: vec![] }.code(),
            AuthzError::DelegationEntityNotCovered { delegation_id: "x".to_string(), entity_id: "x".to_string(), covered: vec![] }.code(),
            AuthzError::DelegationNotYetValid { delegation_id: "x".to_string(), not_before_tai_ns: 0, now_tai_ns: 0 }.code(),
            AuthzError::DelegationExpired { delegation_id: "x".to_string(), expires_tai_ns: 0, now_tai_ns: 0 }.code(),
            AuthzError::MfaRequired { command_class: "x".to_string(), required_amr: vec![], required_acr: "x".to_string(), actual_amr: vec![], actual_acr: "x".to_string() }.code(),
        ];
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "every AuthzError variant must have its own distinct code: {codes:?}");
        for code in &codes {
            assert_eq!(*code, code.to_lowercase(), "codes must be snake_case: {code}");
        }
    }

    /// **The disjointness check itself**, at `load_profile_authz_config` time: a
    /// `service_roles` key that also appears in `roles` is refused, naming the overlapping
    /// role, before any RPC ever gets a chance to consult either table.
    #[test]
    fn load_profile_authz_config_refuses_an_overlapping_role_name() {
        let yaml = r#"
authority:
  roles:
    operators: ["mode"]
  service_roles:
    operators: ["dispatch"]
"#;
        let err = load_profile_authz_config(yaml).unwrap_err();
        match err {
            LoadProfileAuthzConfigError::OverlappingServiceRole(OverlappingServiceRoleError::Overlap { overlapping }) => {
                assert_eq!(overlapping, vec!["operators".to_string()]);
            }
            other => panic!("expected OverlappingServiceRole, got {other:?}"),
        }
    }

    /// Disjoint role tables (no key shared) load cleanly, and `service_roles` is read
    /// correctly alongside every A2.2 field already covered by this file's own
    /// `load_profile_authz_config_reads_the_authority_blocks_a2_2_fields` test.
    #[test]
    fn load_profile_authz_config_accepts_disjoint_role_tables_and_reads_service_roles() {
        let yaml = r#"
authority:
  roles:
    operators: ["mode"]
  service_roles:
    dispatchers: ["dispatch", "ack", "expire", "fail"]
"#;
        let config = load_profile_authz_config(yaml).unwrap();
        assert_eq!(config.roles.get("operators"), Some(&vec!["mode".to_string()]));
        assert_eq!(config.service_roles.get("dispatchers"), Some(&vec!["dispatch".to_string(), "ack".to_string(), "expire".to_string(), "fail".to_string()]));
    }

    /// A missing `service_roles:` key parses as an empty table (deny by default), never an
    /// error and never a wildcard-grants-everything shape -- mirrors this file's own
    /// `load_delegations_file_parses_a_real_document_and_an_empty_one`'s identical convention
    /// for `delegations:`.
    #[test]
    fn load_profile_authz_config_with_no_service_roles_key_is_an_empty_table() {
        let config = load_profile_authz_config("authority:\n  roles: {}\n").unwrap();
        assert!(config.service_roles.is_empty());
    }

    #[test]
    fn load_profile_authz_config_refuses_an_unknown_service_rpc_name() {
        let yaml = "authority:\n  service_roles:\n    dispatchers: [\"reboot\"]\n";
        let err = load_profile_authz_config(yaml).unwrap_err();
        assert!(matches!(err, LoadProfileAuthzConfigError::ServiceRoleConfig(ServiceRoleConfigError::UnknownRpc { .. })), "{err:?}");
    }
}
