//! The clearance-ladder convention -- rank = index into a configured, deployment-ordered list
//! of markings, and a marking absent from the ladder is its own refusal, never a default rank
//! -- applied to `crate::query::find_assets`'s own need: the set of markings a caller's claimed
//! clearance is entitled to see.
//!
//! **This is the workspace's fourth copy of this convention, not the first.**
//! `crates/av-edge/src/policy.rs::ProducerPolicy` originated it; `crates/av-gateway/src/
//! labels.rs::ClearanceLadder` and `crates/av-store/src/labels.rs::ClearanceLadder` are the
//! second and third, near-identical copies -- see `av-store`'s own module doc for exactly why
//! it could not simply depend on one of the earlier two (this task inherits the identical
//! constraints: `av-edge` is off-limits to this whole heavy track per `docs/heavy-plan.md`'s
//! own "Isolation" section, and `av-catalog` must not depend on `av-gateway`/`av-store` --
//! `av-gateway` is the read-path *consumer* sitting above the catalog tier, and `av-store` is a
//! sibling data tier this crate has no reason to couple to). A shared `av-labels`-shaped crate
//! the other three, and this one, all depend on remains a real opportunity -- and this task's
//! own final report names it as a proposal for the manager, exactly as `av-store`'s own module
//! doc already did -- but extracting shared code across four crates this task did not open is
//! not this task's call to make unilaterally.
//!
//! **What this copy needs that the other three do not.** `crate::query::find_assets`'s label
//! filter is a SQL `WHERE marking = ANY($1)`, not a single caller-vs-one-object comparison
//! (`av-gateway`'s `classify`/`av-store`'s `authorize_read`, each judging one `Label` at a
//! time) -- so this module's own entry point, [`ClearanceLadder::markings_at_or_below`],
//! returns the whole SET of markings at or below the caller's rank (every one of them then
//! bound as the ONE array parameter `crate::query::find_assets`'s SQL passes to `= ANY(...)`),
//! rather than judging a single stored marking. The same two rules still hold: a marking
//! absent from the ladder is refused, never defaulted to a rank (here: the CALLER's own
//! marking -- the object side of that rule falls out for free, restated in
//! `crate::query`'s own module doc: a stored `marking` that is not itself one of the values in
//! the returned set, because it is not on the ladder at all, can never match `= ANY(...)`
//! under any caller clearance).

use crate::error::CatalogError;

/// This deployment's ordered clearance ladder: `ladder[i]` outranks `ladder[j]` for every
/// `i > j`. Configured, never hardcoded -- one handling scheme per deployment, spelled however
/// that deployment's customer spells it (`Label.marking`'s own doc comment example: `"CUI"` /
/// `"CUI//SP-EXPT"` / `"UNCLASSIFIED"`).
#[derive(Debug, Clone)]
pub struct ClearanceLadder {
    ladder: Vec<String>,
}

impl ClearanceLadder {
    pub fn new(ladder: Vec<String>) -> Self {
        Self { ladder }
    }

    fn rank(&self, marking: &str) -> Option<usize> {
        self.ladder.iter().position(|m| m == marking)
    }

    /// Every marking at or below `caller_clearance`'s own rank, INCLUDING `caller_clearance`
    /// itself, in ladder order -- the exact parameter `crate::query::find_assets` binds to its
    /// SQL's `marking = ANY($1)`. `Err(CatalogError::CallerMarkingNotOnLadder)` if
    /// `caller_clearance` itself is not on this ladder at all -- refused before any SQL runs
    /// (this module's own doc, and `crate::query`'s own module doc, both state why: a query
    /// this function refused never even reaches the point of asking the database anything, so
    /// no row count, cost, or `LIMIT`-implied existence of an over-clearance row is ever
    /// observable to a caller whose own clearance is not even a valid marking).
    pub fn markings_at_or_below(&self, caller_clearance: &str) -> Result<Vec<String>, CatalogError> {
        let rank = self.rank(caller_clearance).ok_or_else(|| CatalogError::CallerMarkingNotOnLadder { marking: caller_clearance.to_string() })?;
        Ok(self.ladder[..=rank].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ladder() -> ClearanceLadder {
        ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()])
    }

    #[test]
    fn markings_at_or_below_top_rank_returns_the_whole_ladder_in_order() {
        assert_eq!(ladder().markings_at_or_below("SECRET").unwrap(), vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]);
    }

    #[test]
    fn markings_at_or_below_bottom_rank_returns_only_itself() {
        assert_eq!(ladder().markings_at_or_below("UNCLASSIFIED").unwrap(), vec!["UNCLASSIFIED".to_string()]);
    }

    #[test]
    fn markings_at_or_below_middle_rank_returns_itself_and_everything_below() {
        assert_eq!(ladder().markings_at_or_below("CUI").unwrap(), vec!["UNCLASSIFIED".to_string(), "CUI".to_string()]);
    }

    #[test]
    fn markings_at_or_below_refuses_a_caller_clearance_absent_from_the_ladder() {
        let err = ladder().markings_at_or_below("TOP-SECRET").unwrap_err();
        assert!(matches!(&err, CatalogError::CallerMarkingNotOnLadder { marking } if marking == "TOP-SECRET"), "{err:?}");
    }

    #[test]
    fn markings_at_or_below_never_returns_a_marking_above_the_callers_rank() {
        let result = ladder().markings_at_or_below("CUI").unwrap();
        assert!(!result.contains(&"SECRET".to_string()), "{result:?}");
    }
}
