//! The clearance-ladder convention -- rank = index into a configured, deployment-ordered
//! list of markings, and a marking absent from the ladder is its own refusal, never a
//! default rank -- applied to this crate's own comparison: a reading principal's claimed
//! clearance against the object's own stored [`Label`].
//!
//! **This is the THIRD copy of this convention in the workspace**, not the first:
//! `crates/av-edge/src/policy.rs::ProducerPolicy` originated it (a producer's declared
//! clearance against its own ladder), and `crates/av-gateway/src/labels.rs::ClearanceLadder`
//! is the second, near-identical copy (a caller's clearance against a run product's label --
//! this module's own shape is closest to that one, including the name). This crate cannot
//! depend on either existing copy to avoid a third reimplementation: `av-edge` is off-limits
//! to this track entirely (`docs/heavy-plan.md`'s "Isolation" section: "It does not edit ...
//! the edge crates", and this crate must not even *depend on* one it cannot edit, since a
//! future edge-side change to `ProducerPolicy` would then silently reach into this store's
//! own read authorization with no review path this track owns); depending on `av-gateway`
//! would invert this workspace's intended layering (`av-gateway` is the read-path *consumer*
//! sitting above services like this one, per `docs/heavy-plan.md`'s own architecture
//! reference -- a store crate depending on a gateway crate would be the dependency arrow
//! pointing backwards).
//!
//! A shared home for this convention (a small `av-labels`-shaped crate the other two, and
//! this one, all depend on) is a real opportunity, but it is **a proposal for the manager**,
//! not something this task creates unilaterally -- extracting shared code across three
//! crates this task is not otherwise touching (`av-edge`, `av-gateway`) is exactly the kind
//! of workspace-wide refactor a single task brief should not decide on its own recognizance.
//! This task's own report names the proposal explicitly.

use av_cdm::pb::Label;

use crate::error::{Side, StoreError};

/// This deployment's ordered clearance ladder: `ladder[i]` outranks `ladder[j]` for every
/// `i > j`. Configured, never hardcoded -- see this module's own doc for why (one handling
/// scheme per deployment, spelled however that deployment's customer spells it).
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

    /// Refuses a read of an object labelled `object_label` by a principal claiming
    /// `caller_clearance`, unless `caller_clearance`'s rank is at or above `object_label`'s
    /// rank on this ladder. Mislabeling (either side absent from the ladder) is checked
    /// before over-clearance, and the caller's own side is checked before the object's --
    /// both exactly matching `crates/av-gateway/src/labels.rs::ClearanceLadder::classify`'s
    /// documented order, which this module's own doc explains this crate could not reuse by
    /// dependency but still copies by convention.
    pub fn authorize_read(&self, caller_clearance: &str, object_label: &Label) -> Result<(), StoreError> {
        let Some(caller_rank) = self.rank(caller_clearance) else {
            return Err(StoreError::MarkingNotOnLadder { side: Side::Caller, marking: caller_clearance.to_string() });
        };
        let Some(object_rank) = self.rank(&object_label.marking) else {
            return Err(StoreError::MarkingNotOnLadder { side: Side::Object, marking: object_label.marking.clone() });
        };
        if object_rank > caller_rank {
            return Err(StoreError::OverClearance { object_marking: object_label.marking.clone(), caller_clearance: caller_clearance.to_string() });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ladder() -> ClearanceLadder {
        ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()])
    }

    fn label(marking: &str) -> Label {
        Label { marking: marking.to_string(), caveats: vec![] }
    }

    #[test]
    fn authorize_read_allows_a_caller_at_or_above_the_objects_rank() {
        ladder().authorize_read("SECRET", &label("CUI")).unwrap();
        ladder().authorize_read("CUI", &label("CUI")).unwrap();
        ladder().authorize_read("UNCLASSIFIED", &label("UNCLASSIFIED")).unwrap();
    }

    #[test]
    fn authorize_read_refuses_over_clearance() {
        let err = ladder().authorize_read("UNCLASSIFIED", &label("SECRET")).unwrap_err();
        let repr = format!("{err:?}");
        assert!(matches!(err, StoreError::OverClearance { object_marking, caller_clearance } if object_marking == "SECRET" && caller_clearance == "UNCLASSIFIED"), "{repr}");
    }

    #[test]
    fn authorize_read_refuses_an_object_marking_absent_from_the_ladder_even_for_a_fully_cleared_caller() {
        let err = ladder().authorize_read("SECRET", &label("TOP-SECRET")).unwrap_err();
        let repr = format!("{err:?}");
        assert!(matches!(err, StoreError::MarkingNotOnLadder { side: Side::Object, marking } if marking == "TOP-SECRET"), "{repr}");
    }

    #[test]
    fn authorize_read_refuses_a_caller_clearance_absent_from_the_ladder() {
        let err = ladder().authorize_read("TOP-SECRET", &label("CUI")).unwrap_err();
        let repr = format!("{err:?}");
        assert!(matches!(err, StoreError::MarkingNotOnLadder { side: Side::Caller, marking } if marking == "TOP-SECRET"), "{repr}");
    }

    /// Pins the order (caller checked before object), the way
    /// `crates/av-gateway/src/labels.rs`'s own identically-named test does: both sides are
    /// off-ladder, so only checking the caller side first can produce this specific refusal.
    #[test]
    fn authorize_read_checks_caller_marking_before_object_marking() {
        let err = ladder().authorize_read("NOT-ON-LADDER-CALLER", &label("ALSO-NOT-ON-LADDER-OBJECT")).unwrap_err();
        let repr = format!("{err:?}");
        assert!(matches!(err, StoreError::MarkingNotOnLadder { side: Side::Caller, marking } if marking == "NOT-ON-LADDER-CALLER"), "{repr}");
    }
}
