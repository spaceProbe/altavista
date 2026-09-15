//! R5.1 (`docs/open-questions.md` question 208(b), the lead's ruling): every caller of this
//! crate's four surfaces -- gRPC `DataGatewayService.Query`, gRPC `ModelProposeService.
//! ProposeCommand`, the MCP `query`/`propose_command` tools, and the admin `GET /admin/api/
//! evidence/bundle` route -- is now authenticated: a verified OIDC token, never a caller-
//! supplied string trusted as identity or as clearance. This module is the ONE place that
//! verification happens; every surface in [`crate::gateway`], [`crate::propose_flow`],
//! [`crate::mcp`] and [`crate::admin`] calls through [`AuthContext`], never a second copy of
//! any check below.
//!
//! # Reuse, never duplicate, the verifier (invariant A)
//!
//! Token verification itself is exactly [`av_command::oidc::verify`] against an
//! [`av_command::oidc::IssuerConfig`] this process loads once at startup -- the identical path
//! `AuthorizeRequest.principal_token` and `DispatchRequest.service_token` already take in
//! `av-command`. There is no JWT/JWS parsing, no signature check and no crypto of any kind in
//! this module; every byte of that logic lives in `av-command` and is reused as-is.
//!
//! # Two role tables, not `av_command::authz::ServiceRoleTable`
//!
//! `av_command::authz::ServiceRoleTable` gates exactly four fixed RPC names (`dispatch`/`ack`/
//! `expire`/`fail`, [`av_command::authz::ServiceRpc`]'s own closed enum) -- this crate's own
//! surfaces are named differently (`"query"`/`"propose"`/`"admin_bundle"`, [`Surface`]) and
//! `ServiceRpc::parse` has no way to express them. Rather than widen that enum for a crate it
//! was never written for, this module reuses [`av_command::authz::RoleTable`] itself TWICE --
//! once for human roles, once for service roles -- passing each [`Surface::name`] in exactly
//! the position `RoleTable::granting_role` already takes a `command_class` string. This is
//! real reuse (the identical `role -> Vec<granted name>` lookup, the identical [`av_command::
//! authz::WILDCARD`] convention, the identical "an absent/empty table denies everything"
//! guarantee), not a second table implementation -- only the *names* being looked up differ
//! from `av-command`'s own `roles`/`service_roles` blocks.
//!
//! # The human/service distinction is structural, not claimed (mirrors `av-command`'s R3.1)
//!
//! A verified token is a SERVICE principal, for a given surface, exactly when a role in
//! [`GatewayAuthConfig::service_roles`] grants that surface; it is a HUMAN principal exactly
//! when a role in [`GatewayAuthConfig::roles`] grants it -- there is no other claim on
//! `Principal` that says which kind a token is (`authority.proto`'s own doc comment on that
//! message explains why not). [`check_role_tables_disjoint`] enforces the same invariant
//! `av_command::authz::check_service_roles_disjoint` enforces for `av-command`'s own two
//! tables, for the identical reason: without it, a human operator's own role could double as
//! the proposer's `"propose"` credential the moment the same role name appeared in both
//! blocks, letting a person's token submit a command AS the automated proposer -- exactly the
//! escalation shape invariant E names.
//!
//! # Clearance is derived from the verified token, never taken from the request (invariant C)
//!
//! [`GroupClearanceMap`] is a second, independent, deployment-configured table -- `group ->
//! marking`, `BTreeMap` (ADR-004's determinism rule) -- mirroring [`crate::labels::
//! ClearanceLadder`]'s own "explicit, ordered/configured, never a hardcoded enum or numeric
//! level" convention. [`GroupClearanceMap::clearance_for`] returns the marking for the FIRST
//! of the principal's groups (claim order, `av_command::oidc`'s own convention -- this module
//! never re-sorts it) that the map lists; a principal none of whose groups appear in the map
//! is refused [`AuthRefusal::NoClearanceForSubject`], deny by default. The caller-supplied
//! `GatewayQueryRequest.caller_clearance` is NEVER itself trusted: when non-empty it is
//! checked for exact equality against the token-derived marking and refused
//! [`AuthRefusal::ClearanceMismatch`] on any disagreement (never silently taking the higher
//! value, and never silently taking the lower one either); when empty, the token-derived
//! marking is used as this query's effective `caller_clearance` going into
//! [`crate::gateway::GatewayCore::query`] unchanged.
//!
//! # Fail closed (invariant B)
//!
//! [`AuthContext::new`] takes an already-built, already-loaded [`av_command::oidc::
//! IssuerConfig`] -- there is no `Option`, no "unconfigured" state this type can be in.
//! `crates/av-gateway/src/bin/av-gateway.rs` requires `--oidc-issuer`/`--oidc-audience`/
//! `--oidc-public-key-path` at startup (mirroring `av-command`'s own binary's identical, never-
//! defaulted three flags) and refuses to start without all three -- there is no code path in
//! this binary that reaches `tonic::transport::Server::serve`, `crate::mcp::serve` or
//! `crate::admin::serve` with no issuer configuration. Every one of the four surfaces below
//! authenticates BEFORE touching any product data, any ledger record, or any downstream RPC.

use std::collections::BTreeMap;
use std::sync::Arc;

use av_cdm::pb::Principal;
use thiserror::Error;

use av_command::authz::RoleTable;
use av_command::clock::Clock;
use av_command::oidc::{verify, IssuerConfig, TokenError};

use crate::counters::{Counted, Counters};

/// This crate's own authenticated surfaces -- the "granted name" [`RoleTable::granting_role`]
/// looks up, exactly where `av-command`'s own `roles`/`service_roles` blocks list a command
/// class or a [`av_command::authz::ServiceRpc`] name. Adding a surface means adding a variant
/// here AND to [`Self::ALL`] -- no second place a name needs registering, mirroring
/// `crate::mcp::GatewayTool::ALL`'s own "one source" discipline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// gRPC `DataGatewayService.Query` and the MCP `query` tool (both call through
    /// [`crate::gateway::GatewayCore::query`]) -- granted by EITHER role table (see
    /// [`AuthContext::authenticate_query`]'s own doc for why): a human/console role, or the
    /// proposer's own service role (it reads data before it ever proposes).
    Query,
    /// gRPC `ModelProposeService.ProposeCommand` and the MCP `propose_command` tool (both call
    /// through [`crate::propose_flow::propose_command`]) -- the proposer's own service role.
    Propose,
    /// `GET /admin/api/evidence/bundle` -- an administrator's own role.
    AdminBundle,
}

impl Surface {
    pub const ALL: &'static [Surface] = &[Surface::Query, Surface::Propose, Surface::AdminBundle];

    pub fn name(self) -> &'static str {
        match self {
            Surface::Query => "query",
            Surface::Propose => "propose",
            Surface::AdminBundle => "admin_bundle",
        }
    }
}

impl std::fmt::Display for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// This deployment's own `role -> [group names]`-keyed configuration, YAML, read once at
/// process startup (`crates/av-gateway/src/bin/av-gateway.rs`) -- deliberately NOT a reuse of
/// `av_command::authz::ProfileAuthzConfig` (that type also carries `mfa_amr_methods`/`mfa_acr`/
/// `delegations_path`, none of which this crate has any use for; a config TYPE is plain data,
/// not the verifier, so a second, narrower one here is not a violation of invariant A). Every
/// map is a `BTreeMap` (ADR-004): deterministic iteration, and -- more importantly for [`load_gateway_auth_config`]'s disjointness check -- a deterministic error message when it fails.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct GatewayAuthConfig {
    /// Human role -> the [`Surface`] names it grants (by [`Surface::name`], or [`WILDCARD`]
    /// for every surface). Deny by default: a role absent here, or an empty block, grants
    /// nothing -- [`RoleTable::granting_role`]'s own guarantee.
    #[serde(default)]
    pub roles: BTreeMap<String, Vec<String>>,
    /// Service role -> the [`Surface`] names it grants. Checked disjoint from [`Self::roles`]
    /// by [`load_gateway_auth_config`] -- see this module's own doc for why.
    #[serde(default)]
    pub service_roles: BTreeMap<String, Vec<String>>,
    /// Group name -> the clearance marking that group asserts (checked, in claim order, by
    /// [`GroupClearanceMap::clearance_for`]). A group absent here asserts no clearance.
    #[serde(default)]
    pub group_clearance: BTreeMap<String, String>,
}

/// Mirrors `av_command::authz::OverlappingServiceRoleError` exactly (same reasoning, same
/// shape) for THIS crate's own two role tables -- see the module doc's "human/service
/// distinction" section for why this check exists at all.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GatewayAuthConfigError {
    #[error(
        "roles and service_roles must be disjoint, but role(s) {overlapping:?} appear in both \
         -- a human operator's group must never silently double as a query/propose credential \
         reserved for the automated proposer, and a token minted for a person must never be \
         able to submit a command as the service"
    )]
    Overlap { overlapping: Vec<String> },
}

fn check_role_tables_disjoint(roles: &BTreeMap<String, Vec<String>>, service_roles: &BTreeMap<String, Vec<String>>) -> Result<(), GatewayAuthConfigError> {
    let overlapping: Vec<String> = service_roles.keys().filter(|k| roles.contains_key(*k)).cloned().collect();
    if overlapping.is_empty() {
        Ok(())
    } else {
        Err(GatewayAuthConfigError::Overlap { overlapping })
    }
}

/// Parses this crate's own auth YAML (`roles:`/`service_roles:`/`group_clearance:` top-level
/// keys) and validates the one load-time invariant this module has: `roles` and
/// `service_roles` must be disjoint. A missing or empty file's worth of any block parses as an
/// empty map (`#[serde(default)]`), never a parse error and never a wildcard-grants-everything
/// table -- an empty [`GatewayAuthConfig`] denies every surface to every caller, the same
/// "no code path starts from allow" guarantee `RoleTable::granting_role` already gives.
pub fn load_gateway_auth_config(yaml: &str) -> Result<GatewayAuthConfig, GatewayAuthConfigLoadError> {
    let config: GatewayAuthConfig = serde_yaml::from_str(yaml)?;
    check_role_tables_disjoint(&config.roles, &config.service_roles)?;
    Ok(config)
}

#[derive(Debug, Error)]
pub enum GatewayAuthConfigLoadError {
    #[error("parsing gateway auth config: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error(transparent)]
    Overlap(#[from] GatewayAuthConfigError),
}

/// `group -> clearance marking`, deployment-configured (see the module doc's "Clearance is
/// derived from the verified token" section). A thin `BTreeMap` wrapper, not a second
/// `ClearanceLadder` -- this map does not rank markings against each other (that is still
/// entirely [`crate::labels::ClearanceLadder`]'s own job, run afterward, unchanged); it only
/// answers "what does this verified token assert its clearance to be," the fact [`crate::
/// labels::ClearanceLadder::classify`] then ranks against the product's own label.
#[derive(Debug, Clone, Default)]
pub struct GroupClearanceMap {
    by_group: BTreeMap<String, String>,
}

impl GroupClearanceMap {
    pub fn new(by_group: BTreeMap<String, String>) -> Self {
        Self { by_group }
    }

    /// The clearance marking for the FIRST of `groups` (claim order) present in this map, or
    /// `None` if none of them are -- deny by default, never a rank-0/default marking.
    pub fn clearance_for<'a>(&'a self, groups: &[String]) -> Option<&'a str> {
        groups.iter().find_map(|g| self.by_group.get(g)).map(String::as_str)
    }
}

/// Every way [`AuthContext`] can refuse a caller. Distinct, greppable, `Counted` codes --
/// classifiable by the CODE, never by message prose (round 4's decision 2, restated for this
/// track's newest refusal family).
#[derive(Debug, Error)]
pub enum AuthRefusal {
    /// No token at all (an empty string) -- refused the same way a malformed one is, never
    /// treated as "no credential presented, so allow."
    #[error("no caller_token/authorization presented for surface {surface} -- an absent token is never treated as allow")]
    MissingToken { surface: Surface },
    /// The presented token failed [`av_command::oidc::verify`] -- the specific reason survives
    /// in this variant's own `TokenError`, and (see [`AuthContext::verify_token`]) the
    /// underlying `TokenError`'s own specific counter code is recorded ALONGSIDE this
    /// variant's generic one, so both a coarse "some token failed here" count and the precise
    /// `token_*` reason stay independently greppable.
    #[error("caller_token for surface {surface} failed verification: {source}")]
    TokenInvalid { surface: Surface, #[source] source: TokenError },
    /// The verified token's `groups` name no role (on the table this surface consults --
    /// human for [`Surface::Query`]/[`Surface::AdminBundle`], service for [`Surface::
    /// Propose`]) granting this surface. This is also what a purely human token calling
    /// [`Surface::Propose`] gets, and what a purely service token calling [`Surface::Query`]/
    /// [`Surface::AdminBundle`] gets -- by [`check_role_tables_disjoint`]'s own construction,
    /// exactly `av_command::authz::ServiceAuthzError::ServiceRoleNotGranted`'s own reasoning.
    #[error("no role in groups {groups:?} grants surface {surface} (crate::auth, deny-by-default)")]
    RoleNotGranted { groups: Vec<String>, surface: Surface },
    /// [`Surface::Query`] only: none of the verified token's groups appear in this
    /// deployment's [`GatewayAuthConfig::group_clearance`] table -- the token asserts no
    /// clearance this deployment recognizes, refused before ranking against any product label.
    #[error("no configured clearance for groups {groups:?} -- this deployment's group_clearance table names none of them")]
    NoClearanceForSubject { groups: Vec<String> },
    /// [`Surface::Query`] only: `GatewayQueryRequest.caller_clearance` was non-empty and
    /// disagreed with the token-derived clearance. Never resolved by silently preferring
    /// either value (invariant C) -- always this typed, counted refusal instead.
    #[error("declared caller_clearance {declared:?} disagrees with the verified, token-derived clearance {verified:?}")]
    ClearanceMismatch { declared: String, verified: String },
    /// [`Surface::Propose`] only: `ProposeCommandRequest.principal` (or the MCP tool's
    /// `principal` argument) was non-empty and disagreed with the verified `Principal.sub`.
    /// Mirrors `av_command::service::CommandAuthorityServiceImpl::check_declared_principal`'s
    /// own rule for `AckRequest.principal` verbatim, restated at this crate's own boundary.
    #[error("declared principal {declared:?} disagrees with the verified service subject {verified:?}")]
    PrincipalMismatch { declared: String, verified: String },
}

impl Counted for AuthRefusal {
    fn code(&self) -> &'static str {
        match self {
            AuthRefusal::MissingToken { .. } => "gateway_auth_missing_token",
            AuthRefusal::TokenInvalid { .. } => "gateway_auth_token_invalid",
            AuthRefusal::RoleNotGranted { .. } => "gateway_auth_role_not_granted",
            AuthRefusal::NoClearanceForSubject { .. } => "gateway_auth_no_clearance_for_subject",
            AuthRefusal::ClearanceMismatch { .. } => "gateway_auth_clearance_mismatch",
            AuthRefusal::PrincipalMismatch { .. } => "gateway_auth_principal_mismatch",
        }
    }
}

/// Everything every surface's authentication check needs: the issuer configuration (always
/// present -- see the module doc's "Fail closed" section), the human and service role tables,
/// the group-clearance table, and the injected clock (invariant J: `oidc::verify` takes a
/// clock *reading*, never `SystemTime::now()`).
pub struct AuthContext {
    issuer_config: Arc<IssuerConfig>,
    human_roles: Arc<RoleTable>,
    service_roles: Arc<RoleTable>,
    group_clearance: Arc<GroupClearanceMap>,
    clock: Arc<dyn Clock>,
}

impl AuthContext {
    pub fn new(issuer_config: Arc<IssuerConfig>, human_roles: Arc<RoleTable>, service_roles: Arc<RoleTable>, group_clearance: Arc<GroupClearanceMap>, clock: Arc<dyn Clock>) -> Self {
        Self { issuer_config, human_roles, service_roles, group_clearance, clock }
    }

    /// Verifies `token` for `surface`. Empty is [`AuthRefusal::MissingToken`]; otherwise
    /// [`av_command::oidc::verify`] runs, and on failure the underlying [`TokenError`]'s own
    /// specific counter is recorded (see [`AuthRefusal::TokenInvalid`]'s own doc) in addition
    /// to the generic `gateway_auth_token_invalid` one this function's own caller records.
    fn verify_token(&self, token: &str, surface: Surface, counters: &Counters) -> Result<Principal, AuthRefusal> {
        if token.is_empty() {
            return Err(AuthRefusal::MissingToken { surface });
        }
        let now_tai_ns = self.clock.now_tai_ns();
        verify(token, &self.issuer_config, now_tai_ns).map_err(|source| {
            counters.record(&source);
            AuthRefusal::TokenInvalid { surface, source }
        })
    }

    fn refuse(&self, refusal: AuthRefusal, counters: &Counters) -> AuthRefusal {
        counters.record(&refusal);
        refusal
    }

    /// [`Surface::Query`]'s full gate: verify -> role check (human OR service table) ->
    /// clearance derivation -> clearance-agreement check. Returns the verified principal and
    /// the query's EFFECTIVE `caller_clearance` (the token-derived marking; `declared_
    /// clearance` has already been checked to agree with it, or was empty) -- callers pass
    /// this value, never `declared_clearance` itself, into [`crate::gateway::GatewayCore::
    /// query`].
    ///
    /// **Both role tables, deliberately** -- unlike [`Self::authenticate_propose`] (service-
    /// only) and [`Self::authenticate_admin_bundle`] (human-only), `"query"` is a surface BOTH
    /// kinds of principal legitimately call: a console operator reads product data directly,
    /// and the proposer (a service principal) reads scores via `DataGatewayService.Query`
    /// BEFORE it ever calls `ProposeCommand` (D5's own "what the model saw" evidence needs a
    /// real, authenticated query first) -- `profiles/gateway-authority.yaml`'s own shipped
    /// `service_roles.proposer-service` entry grants both `"query"` and `"propose"` for
    /// exactly this reason. A role granting `"query"` on either table is sufficient; this does
    /// NOT blur the human/service distinction invariant E requires, because that distinction
    /// is enforced at the SERVICE-only [`Self::authenticate_propose`] gate, never here.
    pub fn authenticate_query(&self, token: &str, declared_clearance: &str, counters: &Counters) -> Result<(Principal, String), AuthRefusal> {
        let principal = self.verify_token(token, Surface::Query, counters).map_err(|e| self.refuse(e, counters))?;

        let granted = self.human_roles.granting_role(&principal.groups, Surface::Query.name()).is_some()
            || self.service_roles.granting_role(&principal.groups, Surface::Query.name()).is_some();
        if !granted {
            return Err(self.refuse(AuthRefusal::RoleNotGranted { groups: principal.groups.clone(), surface: Surface::Query }, counters));
        }

        let verified_clearance = match self.group_clearance.clearance_for(&principal.groups) {
            Some(m) => m.to_string(),
            None => return Err(self.refuse(AuthRefusal::NoClearanceForSubject { groups: principal.groups.clone() }, counters)),
        };

        if !declared_clearance.is_empty() && declared_clearance != verified_clearance {
            return Err(self.refuse(AuthRefusal::ClearanceMismatch { declared: declared_clearance.to_string(), verified: verified_clearance }, counters));
        }

        Ok((principal, verified_clearance))
    }

    /// [`Surface::Propose`]'s full gate: verify -> service role check -> declared-principal
    /// agreement. Returns the verified principal; callers record its `sub` (never `declared_
    /// principal`) as `ProposalEvidence.model_identity` (invariant D).
    pub fn authenticate_propose(&self, token: &str, declared_principal: &str, counters: &Counters) -> Result<Principal, AuthRefusal> {
        let principal = self.verify_token(token, Surface::Propose, counters).map_err(|e| self.refuse(e, counters))?;

        if self.service_roles.granting_role(&principal.groups, Surface::Propose.name()).is_none() {
            return Err(self.refuse(AuthRefusal::RoleNotGranted { groups: principal.groups.clone(), surface: Surface::Propose }, counters));
        }

        if !declared_principal.is_empty() && declared_principal != principal.sub {
            return Err(self.refuse(AuthRefusal::PrincipalMismatch { declared: declared_principal.to_string(), verified: principal.sub.clone() }, counters));
        }

        Ok(principal)
    }

    /// [`Surface::AdminBundle`]'s gate: verify -> human role check. No clearance/principal
    /// step -- the bundle carries no product data (see `crate::evidence_bundle`'s own module
    /// doc on what it does and does not carry), only this process's own refusal counts and
    /// ledger partition summaries.
    pub fn authenticate_admin_bundle(&self, token: &str, counters: &Counters) -> Result<Principal, AuthRefusal> {
        let principal = self.verify_token(token, Surface::AdminBundle, counters).map_err(|e| self.refuse(e, counters))?;

        if self.human_roles.granting_role(&principal.groups, Surface::AdminBundle.name()).is_none() {
            return Err(self.refuse(AuthRefusal::RoleNotGranted { groups: principal.groups.clone(), surface: Surface::AdminBundle }, counters));
        }

        Ok(principal)
    }
}

/// Maps an [`AuthRefusal`] to a [`tonic::Status`] code -- "who are you" is `UNAUTHENTICATED`,
/// "you may not" is `PERMISSION_DENIED` (invariant F). The gRPC surfaces
/// ([`crate::gateway`]/[`crate::propose_flow`]) use this directly; the MCP surface
/// ([`crate::mcp`]) and the admin surface ([`crate::admin`]) map the same variants to their
/// own JSON-RPC/HTTP codes instead, never re-deriving the classification from message prose.
pub fn to_status(refusal: AuthRefusal) -> tonic::Status {
    let message = refusal.to_string();
    match &refusal {
        AuthRefusal::MissingToken { .. } | AuthRefusal::TokenInvalid { .. } => tonic::Status::unauthenticated(message),
        AuthRefusal::RoleNotGranted { .. } | AuthRefusal::NoClearanceForSubject { .. } | AuthRefusal::ClearanceMismatch { .. } | AuthRefusal::PrincipalMismatch { .. } => {
            tonic::Status::permission_denied(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use av_command::authz::WILDCARD;
    use av_command::clock::TestClock;
    use av_command::test_support::{valid_claims, TestIssuer};

    const ISSUER: &str = "https://sso.test.example/";
    const AUDIENCE: &str = "av-gateway";
    const NOW_UNIX_S: i64 = 1_760_000_000;
    const TTL_S: i64 = 3_600;

    fn issuer_config(issuer: &TestIssuer) -> Arc<IssuerConfig> {
        Arc::new(IssuerConfig::from_public_key_pem(ISSUER, AUDIENCE, issuer.public_key_pem()).unwrap())
    }

    fn roles(entries: &[(&str, &[&str])]) -> Arc<RoleTable> {
        let mut m = BTreeMap::new();
        for (role, surfaces) in entries {
            m.insert(role.to_string(), surfaces.iter().map(|s| s.to_string()).collect());
        }
        Arc::new(RoleTable::from_config(&m))
    }

    fn clearance(entries: &[(&str, &str)]) -> Arc<GroupClearanceMap> {
        let mut m = BTreeMap::new();
        for (group, marking) in entries {
            m.insert(group.to_string(), marking.to_string());
        }
        Arc::new(GroupClearanceMap::new(m))
    }

    fn ctx(issuer: &TestIssuer, human: &[(&str, &[&str])], service: &[(&str, &[&str])], group_clearance: &[(&str, &str)]) -> AuthContext {
        AuthContext::new(issuer_config(issuer), roles(human), roles(service), clearance(group_clearance), Arc::new(TestClock::new(NOW_UNIX_S * 1_000_000_000)))
    }

    fn mint(issuer: &TestIssuer, groups: &[&str]) -> String {
        let mut claims = valid_claims(ISSUER, AUDIENCE, "operator-1", NOW_UNIX_S, TTL_S);
        claims["groups"] = serde_json::json!(groups);
        issuer.mint(&claims)
    }

    // ---- fail-closed: no token, ever, means allow ----

    #[test]
    fn an_empty_query_token_is_refused_missing_token_and_counted_never_served() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[("operators", &["query"])], &[], &[]);
        let counters = Counters::new();
        let err = ctx.authenticate_query("", "", &counters).unwrap_err();
        assert!(matches!(err, AuthRefusal::MissingToken { surface: Surface::Query }), "{err:?}");
        assert_eq!(counters.get("gateway_auth_missing_token"), 1);
    }

    #[test]
    fn an_empty_propose_token_is_refused_missing_token_and_counted_never_served() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[], &[("proposer", &["propose"])], &[]);
        let counters = Counters::new();
        let err = ctx.authenticate_propose("", "", &counters).unwrap_err();
        assert!(matches!(err, AuthRefusal::MissingToken { surface: Surface::Propose }), "{err:?}");
        assert_eq!(counters.get("gateway_auth_missing_token"), 1);
    }

    /// **The fail-open regression test (invariant B).** A syntactically well-formed but
    /// entirely UNSIGNED/garbage string must never verify -- proves this module never has a
    /// branch that treats "a token was present" as sufficient on its own. Watched failing
    /// against a deliberately fail-open `authenticate_query` (one that skipped the `verify`
    /// call and went straight to the role check) before the real implementation above was
    /// written -- see this task's own report for the exact failing output.
    #[test]
    fn a_present_but_unverifiable_token_is_refused_never_served() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[("operators", &["query"])], &[], &[("operators", "CUI")]);
        let counters = Counters::new();
        let err = ctx.authenticate_query("not.a.real.token", "", &counters).unwrap_err();
        assert!(matches!(err, AuthRefusal::TokenInvalid { surface: Surface::Query, .. }), "{err:?}");
        assert_eq!(counters.get("gateway_auth_token_invalid"), 1);
    }

    #[test]
    fn an_empty_role_table_denies_every_query_never_allows() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[], &[], &[("operators", "CUI")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"]);
        let err = ctx.authenticate_query(&token, "", &counters).unwrap_err();
        assert!(matches!(err, AuthRefusal::RoleNotGranted { surface: Surface::Query, .. }), "an empty table must never mean allow: {err:?}");
        assert_eq!(counters.get("gateway_auth_role_not_granted"), 1);
    }

    #[test]
    fn a_valid_query_token_with_the_right_role_and_clearance_succeeds() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[("operators", &["query"])], &[], &[("operators", "CUI")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"]);
        let (principal, clearance) = ctx.authenticate_query(&token, "", &counters).unwrap();
        assert_eq!(principal.sub, "operator-1");
        assert_eq!(clearance, "CUI");
    }

    #[test]
    fn a_wildcard_role_grants_query() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[("safety-officers", &[WILDCARD])], &[], &[("safety-officers", "SECRET")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["safety-officers"]);
        ctx.authenticate_query(&token, "", &counters).expect("wildcard role grants every surface");
    }

    /// **Regression test for a real defect found while wiring `av-proposer`'s own integration
    /// tests**: a purely SERVICE-role token (granted `"query"` only on the service table, not
    /// the human one) must still be able to call the Query surface -- the proposer reads
    /// scores before it ever proposes (see [`AuthContext::authenticate_query`]'s own doc for
    /// why this is deliberate, not a human/service blur). The FIRST version of this method
    /// consulted only `human_roles` and refused every service-only token with `RoleNotGranted`
    /// -- caught by `crates/av-proposer/tests/determinism.rs` failing for real against the
    /// actual proposer flow, not by inspection.
    #[test]
    fn a_service_only_role_also_grants_query() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[], &[("proposer-service", &["query", "propose"])], &[("proposer-service", "CUI")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["proposer-service"]);
        let (_, clearance) = ctx.authenticate_query(&token, "", &counters).expect("a service-only role granting query must be accepted");
        assert_eq!(clearance, "CUI");
    }

    #[test]
    fn a_role_granting_a_different_surface_does_not_grant_query() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[("admins", &["admin_bundle"])], &[], &[("admins", "CUI")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["admins"]);
        let err = ctx.authenticate_query(&token, "", &counters).unwrap_err();
        assert!(matches!(err, AuthRefusal::RoleNotGranted { .. }), "{err:?}");
    }

    /// **Invariant C's own required test.** A token whose group maps to CUI (a low
    /// clearance) sends a request claiming `caller_clearance = "SECRET"` (a high one, and the
    /// exact marking a SECRET-labelled product needs) -- refused, and the counter for it
    /// moves. Proves the request field can never buy a HIGHER clearance than the token
    /// actually carries.
    #[test]
    fn a_low_clearance_token_declaring_secret_for_a_secret_product_is_refused_and_the_counter_moves() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[("operators", &["query"])], &[], &[("operators", "CUI")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"]);

        assert_eq!(counters.get("gateway_auth_clearance_mismatch"), 0);
        let err = ctx.authenticate_query(&token, "SECRET", &counters).unwrap_err();
        assert_eq!(err.to_string(), "declared caller_clearance \"SECRET\" disagrees with the verified, token-derived clearance \"CUI\"");
        assert!(matches!(err, AuthRefusal::ClearanceMismatch { declared, verified } if declared == "SECRET" && verified == "CUI"));
        assert_eq!(counters.get("gateway_auth_clearance_mismatch"), 1, "the counter for this exact refusal must have moved");
    }

    /// The mirror direction: a high-clearance token declaring a LOWER `caller_clearance` is
    /// refused too -- never silently taking the lower value either.
    #[test]
    fn a_high_clearance_token_declaring_a_lower_caller_clearance_is_also_refused() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[("safety-officers", &["query"])], &[], &[("safety-officers", "SECRET")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["safety-officers"]);
        let err = ctx.authenticate_query(&token, "CUI", &counters).unwrap_err();
        assert!(matches!(err, AuthRefusal::ClearanceMismatch { .. }), "{err:?}");
    }

    #[test]
    fn an_agreeing_declared_clearance_is_accepted() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[("operators", &["query"])], &[], &[("operators", "CUI")]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"]);
        let (_, clearance) = ctx.authenticate_query(&token, "CUI", &counters).unwrap();
        assert_eq!(clearance, "CUI");
    }

    #[test]
    fn a_token_whose_groups_map_to_no_clearance_is_refused_and_counted() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[("operators", &["query"])], &[], &[]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"]);
        let err = ctx.authenticate_query(&token, "", &counters).unwrap_err();
        assert!(matches!(err, AuthRefusal::NoClearanceForSubject { .. }), "{err:?}");
        assert_eq!(counters.get("gateway_auth_no_clearance_for_subject"), 1);
    }

    // ---- Propose surface: service role + declared-principal agreement ----

    #[test]
    fn a_human_only_token_is_refused_propose_never_a_service_credential() {
        let issuer = TestIssuer::new();
        // "operators" grants "query" on the HUMAN table only -- disjoint from service_roles.
        let ctx = ctx(&issuer, &[("operators", &["query"])], &[("proposer-service", &["propose"])], &[]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"]);
        let err = ctx.authenticate_propose(&token, "", &counters).unwrap_err();
        assert!(matches!(err, AuthRefusal::RoleNotGranted { surface: Surface::Propose, .. }), "a human role must never authorize the service surface: {err:?}");
    }

    #[test]
    fn a_service_token_proposes_and_the_verified_subject_is_returned() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[], &[("proposer-service", &["propose"])], &[]);
        let counters = Counters::new();
        let token = mint(&issuer, &["proposer-service"]);
        let principal = ctx.authenticate_propose(&token, "", &counters).unwrap();
        assert_eq!(principal.sub, "operator-1");
    }

    #[test]
    fn a_disagreeing_declared_principal_is_refused_and_counted() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[], &[("proposer-service", &["propose"])], &[]);
        let counters = Counters::new();
        let token = mint(&issuer, &["proposer-service"]);
        let err = ctx.authenticate_propose(&token, "someone-else", &counters).unwrap_err();
        assert!(matches!(err, AuthRefusal::PrincipalMismatch { .. }), "{err:?}");
        assert_eq!(counters.get("gateway_auth_principal_mismatch"), 1);
    }

    #[test]
    fn an_empty_declared_principal_declares_nothing_and_is_accepted() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[], &[("proposer-service", &["propose"])], &[]);
        let counters = Counters::new();
        let token = mint(&issuer, &["proposer-service"]);
        ctx.authenticate_propose(&token, "", &counters).expect("an empty declared principal must never be a mismatch");
    }

    #[test]
    fn an_agreeing_declared_principal_is_accepted() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[], &[("proposer-service", &["propose"])], &[]);
        let counters = Counters::new();
        let token = mint(&issuer, &["proposer-service"]);
        ctx.authenticate_propose(&token, "operator-1", &counters).expect("an agreeing declared principal must be accepted");
    }

    // ---- Admin bundle surface ----

    #[test]
    fn an_unauthenticated_admin_bundle_call_is_refused_and_counted() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[("admins", &["admin_bundle"])], &[], &[]);
        let counters = Counters::new();
        let err = ctx.authenticate_admin_bundle("", &counters).unwrap_err();
        assert!(matches!(err, AuthRefusal::MissingToken { surface: Surface::AdminBundle }), "{err:?}");
        assert_eq!(counters.get("gateway_auth_missing_token"), 1);
    }

    #[test]
    fn a_query_only_role_does_not_grant_the_admin_bundle() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[("operators", &["query"])], &[], &[]);
        let counters = Counters::new();
        let token = mint(&issuer, &["operators"]);
        let err = ctx.authenticate_admin_bundle(&token, &counters).unwrap_err();
        assert!(matches!(err, AuthRefusal::RoleNotGranted { surface: Surface::AdminBundle, .. }), "{err:?}");
    }

    #[test]
    fn an_admin_role_grants_the_admin_bundle() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[("admins", &["admin_bundle"])], &[], &[]);
        let counters = Counters::new();
        let token = mint(&issuer, &["admins"]);
        ctx.authenticate_admin_bundle(&token, &counters).expect("admin role must grant the admin bundle");
    }

    // ---- config loading ----

    #[test]
    fn load_gateway_auth_config_parses_all_three_blocks() {
        let yaml = "roles:\n  operators: [query]\nservice_roles:\n  proposer-service: [propose]\ngroup_clearance:\n  operators: CUI\n";
        let config = load_gateway_auth_config(yaml).unwrap();
        assert_eq!(config.roles.get("operators").unwrap(), &vec!["query".to_string()]);
        assert_eq!(config.service_roles.get("proposer-service").unwrap(), &vec!["propose".to_string()]);
        assert_eq!(config.group_clearance.get("operators").unwrap(), "CUI");
    }

    #[test]
    fn an_empty_config_parses_to_three_empty_deny_by_default_tables() {
        let config = load_gateway_auth_config("").unwrap();
        assert!(config.roles.is_empty());
        assert!(config.service_roles.is_empty());
        assert!(config.group_clearance.is_empty());
    }

    #[test]
    fn a_role_name_shared_by_both_tables_is_refused_at_load_time() {
        let yaml = "roles:\n  shared: [query]\nservice_roles:\n  shared: [propose]\n";
        let err = load_gateway_auth_config(yaml).unwrap_err();
        assert!(matches!(err, GatewayAuthConfigLoadError::Overlap(GatewayAuthConfigError::Overlap { .. })), "{err:?}");
    }

    // ---- status mapping (invariant F) ----

    #[test]
    fn missing_and_invalid_token_map_to_unauthenticated_everything_else_to_permission_denied() {
        assert_eq!(to_status(AuthRefusal::MissingToken { surface: Surface::Query }).code(), tonic::Code::Unauthenticated);
        let issuer = TestIssuer::new();
        let cfg = IssuerConfig::from_public_key_pem(ISSUER, AUDIENCE, issuer.public_key_pem()).unwrap();
        let token_err = verify("not.a.real.token", &cfg, 0).unwrap_err();
        assert_eq!(to_status(AuthRefusal::TokenInvalid { surface: Surface::Query, source: token_err }).code(), tonic::Code::Unauthenticated);
        assert_eq!(to_status(AuthRefusal::RoleNotGranted { groups: vec![], surface: Surface::Query }).code(), tonic::Code::PermissionDenied);
        assert_eq!(to_status(AuthRefusal::NoClearanceForSubject { groups: vec![] }).code(), tonic::Code::PermissionDenied);
        assert_eq!(to_status(AuthRefusal::ClearanceMismatch { declared: "a".to_string(), verified: "b".to_string() }).code(), tonic::Code::PermissionDenied);
        assert_eq!(to_status(AuthRefusal::PrincipalMismatch { declared: "a".to_string(), verified: "b".to_string() }).code(), tonic::Code::PermissionDenied);
    }

    /// **Invariant G's own required test.** A refused call's error message must never contain
    /// the raw token string -- only the verified/declared claim VALUES that already survive
    /// into `TokenError`'s own `Display` (which itself never carries the raw token, per
    /// `av_command::oidc`'s own module doc), never the compact JWS itself.
    #[test]
    fn a_refusal_message_never_contains_the_raw_token_string() {
        let issuer = TestIssuer::new();
        let ctx = ctx(&issuer, &[("operators", &["query"])], &[], &[("operators", "CUI")]);
        let counters = Counters::new();
        let bad_token = "this-is-the-secret-raw-token-value.segment-two.segment-three-signature";
        let err = ctx.authenticate_query(bad_token, "", &counters).unwrap_err();
        assert!(!err.to_string().contains(bad_token), "refusal message leaked the raw token: {err}");
        assert!(!to_status(err).message().contains(bad_token));
    }
}
