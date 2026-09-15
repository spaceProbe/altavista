//! R5.1/question 208(b): a shared, permissive [`AuthContext`] for the integration test
//! binaries in this crate whose own focus is NOT authentication itself (D1/D2/D4/D5's own
//! acceptance evidence, exercised end to end over real sockets) -- `crates/av-gateway/src/
//! auth.rs`'s own unit tests and `crates/av-gateway/src/{gateway,propose_flow,mcp,admin}.rs`'s
//! own module tests are where the auth gate itself is exercised.
//!
//! A SEPARATE module from `tests/common/mod.rs` (not folded into it) for exactly the reason
//! that module's own doc states: only two of this crate's integration test binaries
//! (`command_trail_run_products.rs`, `propose_flow_agreement.rs`) call `GatewayCore::query`/
//! `ProposeOnlyAuthority::propose` through the AUTHENTICATED wrapper functions and therefore
//! need any of this; every other binary that includes `mod common;` (`propose_only.rs`,
//! `real_run_products.rs`, `ledger_decision_trail_replay.rs`) calls `GatewayCore::query`
//! directly (auth-agnostic) and would fail its own `-D warnings` build on unused items if this
//! content lived in the module they already include for `tmp_dir` alone.
//!
//! Three clearance-named groups (so a test can mint a token at whichever of UNCLASSIFIED/CUI/
//! SECRET its own scenario needs, matching this crate's own fixtures) plus one service group
//! granting `"propose"` -- every one of them real, checked through the real `av_command::
//! oidc::verify` path, never bypassed for these tests either.

use std::collections::BTreeMap;
use std::sync::Arc;

use av_command::authz::RoleTable;
use av_command::clock::{Clock, TestClock};
use av_command::oidc::IssuerConfig;
use av_command::test_support::{valid_claims, TestIssuer};
use av_gateway::auth::{AuthContext, GroupClearanceMap};

pub const TEST_OIDC_ISSUER: &str = "https://sso.test.example/";
pub const TEST_OIDC_AUDIENCE: &str = "av-gateway";
/// A fixed "now" every [`test_auth_context`]/[`mint_token`] call in this crate's integration
/// tests agrees on, so a token minted at this second against a [`test_clock`] reading is always
/// valid, deterministically, with no dependency on the real wall clock (D8; question 199).
pub const TEST_NOW_UNIX_S: i64 = 1_760_000_000;

/// A [`TestClock`] reading matching [`TEST_NOW_UNIX_S`] -- pass to [`test_auth_context`] (or
/// construct a real server's OWN clock from this same reading) so the server's "now" and a
/// [`mint_token`]-minted token's claims always agree.
pub fn test_clock() -> Arc<dyn Clock> {
    Arc::new(TestClock::new(TEST_NOW_UNIX_S * 1_000_000_000))
}

/// A group name for each marking this workspace's own fixtures use, granting `"query"` and
/// `"admin_bundle"` (human table) -- pick the group matching the clearance a test's own
/// scenario needs.
pub const GROUP_UNCLASSIFIED: &str = "test-clearance-unclassified";
pub const GROUP_CUI: &str = "test-clearance-cui";
pub const GROUP_SECRET: &str = "test-clearance-secret";
/// Grants `"propose"` (service table).
pub const GROUP_PROPOSER: &str = "test-proposer-service";

pub fn test_issuer_config(issuer: &TestIssuer) -> Arc<IssuerConfig> {
    Arc::new(IssuerConfig::from_public_key_pem(TEST_OIDC_ISSUER, TEST_OIDC_AUDIENCE, issuer.public_key_pem()).unwrap())
}

pub fn test_auth_context(issuer: &TestIssuer, clock: Arc<dyn Clock>) -> Arc<AuthContext> {
    let mut roles = BTreeMap::new();
    for group in [GROUP_UNCLASSIFIED, GROUP_CUI, GROUP_SECRET] {
        roles.insert(group.to_string(), vec!["query".to_string(), "admin_bundle".to_string()]);
    }
    let mut service_roles = BTreeMap::new();
    service_roles.insert(GROUP_PROPOSER.to_string(), vec!["propose".to_string()]);
    let mut group_clearance = BTreeMap::new();
    group_clearance.insert(GROUP_UNCLASSIFIED.to_string(), "UNCLASSIFIED".to_string());
    group_clearance.insert(GROUP_CUI.to_string(), "CUI".to_string());
    group_clearance.insert(GROUP_SECRET.to_string(), "SECRET".to_string());

    Arc::new(AuthContext::new(
        test_issuer_config(issuer),
        Arc::new(RoleTable::from_config(&roles)),
        Arc::new(RoleTable::from_config(&service_roles)),
        Arc::new(GroupClearanceMap::new(group_clearance)),
        clock,
    ))
}

/// Mints a real, signed token against `issuer` (the same one [`test_auth_context`] was built
/// from), carrying `groups` -- pick from [`GROUP_UNCLASSIFIED`]/[`GROUP_CUI`]/[`GROUP_SECRET`]/
/// [`GROUP_PROPOSER`], or combine more than one (a single test token carrying both a clearance
/// group and the proposer group is a real, valid shape: nothing about `RoleTable`'s own lookup
/// forbids a principal's groups from spanning both tables -- only the ROLE NAMES on each table
/// must never collide, `crate::auth::check_role_tables_disjoint`'s own job, unrelated to what a
/// given token's own group *membership* looks like).
pub fn mint_token(issuer: &TestIssuer, subject: &str, groups: &[&str], now_unix_s: i64, ttl_s: i64) -> String {
    let mut claims = valid_claims(TEST_OIDC_ISSUER, TEST_OIDC_AUDIENCE, subject, now_unix_s, ttl_s);
    claims["groups"] = serde_json::json!(groups);
    issuer.mint(&claims)
}
