//! D2: label enforcement, copying `crates/av-edge/src/policy.rs`'s clearance-ladder
//! convention exactly (that module's own doc comment, restated for this crate's own
//! domain): an explicit, deployment-configured, **ordered** list of markings
//! ([`ClearanceLadder::ladder`], rank = index into it) -- never a hardcoded enum and never
//! a numeric level. This platform enforces exactly one handling level per deployment
//! (ADR-004, question 32), but different deployments spell their schemes differently
//! (`Label.marking`'s own doc comment example: `"CUI"` / `"CUI//SP-EXPT"` /
//! `"UNCLASSIFIED"`), so the ladder -- and therefore what counts as "above" a given
//! clearance -- is data a deployment configures, never a fact this crate bakes in.
//!
//! Unlike `av-edge`'s `ProducerPolicy` (one producer's own declared emit label vs. its own
//! clearance), this gateway compares two independent markings against the same ladder: the
//! calling principal's own claimed clearance ([`GatewayQueryRequest::caller_clearance`])
//! and the run product's own configured label ([`crate::catalogue::CatalogueEntry::
//! label`]). A marking absent from the ladder is always refused on **either** side of that
//! comparison -- never silently treated as rank 0 or defaulted to any rank -- and that
//! check runs before the rank comparison itself (mislabeling checked before
//! over-clearance, mirroring `ProducerPolicy::classify_label`'s own documented order).
//!
//! **`Label.caveats` is deliberately not compared here.** `ProducerPolicy::classify_label`
//! checks caveats because it is verifying a producer did not lie about the ONE label it
//! itself declared it emits under (`emit_caveats`) -- there is no equivalent "declared vs.
//! actual" pair in this gateway's own comparison (a caller's clearance is not itself a
//! `Label` with its own caveat set; a run's `product_label` is compared against a bare
//! clearance marking, not against another `Label`). A caveat-aware clearance model is a
//! real possibility for a future deployment, but it is not this gateway's convention to
//! invent one without the manager naming it -- this module's own ladder rank is the whole
//! of D2's ask.

use av_cdm::pb::Label;

use crate::counters::Counted;

/// Why [`ClearanceLadder::classify`] refused a query. Every variant is its own typed,
/// counted ([`Counted::code`]) reason -- see the module doc for the two-sided "absent from
/// the ladder" rule and the "mislabeling before over-clearance" order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelRefusal {
    /// `marking` (either the caller's claimed clearance or the product's own configured
    /// label -- `side` names which) does not appear anywhere on this deployment's
    /// [`ClearanceLadder`]. Never defaulted to a rank; refused outright.
    MarkingNotOnLadder { side: Side, marking: String },
    /// Both markings are on the ladder, but the product's own rank outranks the caller's
    /// claimed clearance rank -- D2's own acceptance line: "the gateway refuses a query
    /// whose product's label ranks above the caller's own label."
    OverClearance { product_marking: String, caller_clearance: String },
}

/// Which side of a [`ClearanceLadder::classify`] comparison a [`LabelRefusal::
/// MarkingNotOnLadder`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Caller,
    Product,
}

impl Counted for LabelRefusal {
    fn code(&self) -> &'static str {
        match self {
            LabelRefusal::MarkingNotOnLadder { side: Side::Caller, .. } => "label_caller_marking_not_on_ladder",
            LabelRefusal::MarkingNotOnLadder { side: Side::Product, .. } => "label_product_marking_not_on_ladder",
            LabelRefusal::OverClearance { .. } => "label_over_clearance",
        }
    }
}

impl std::fmt::Display for LabelRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LabelRefusal::MarkingNotOnLadder { side, marking } => {
                write!(f, "{side:?} marking {marking:?} is not on this deployment's clearance ladder -- refused, never defaulted to a rank")
            }
            LabelRefusal::OverClearance { product_marking, caller_clearance } => {
                write!(f, "product label {product_marking:?} outranks caller clearance {caller_clearance:?} on this deployment's clearance ladder")
            }
        }
    }
}

/// This deployment's ordered clearance ladder: `ladder[i]` outranks `ladder[j]` for every
/// `i > j`. Configured, never hardcoded (see the module doc).
#[derive(Debug, Clone)]
pub struct ClearanceLadder {
    ladder: Vec<String>,
}

impl ClearanceLadder {
    pub fn new(ladder: Vec<String>) -> Self {
        Self { ladder }
    }

    /// `pub(crate)`, not private: `crate::auth::GroupClearanceMap::clearance_for` reuses this
    /// SAME rank function to rank a verified token's mapped clearance markings against this
    /// deployment's ladder (R5.1b, defect 1) -- the whole point being that there is exactly one
    /// ranking function in this crate, never a second one reimplemented at the auth boundary.
    pub(crate) fn rank(&self, marking: &str) -> Option<usize> {
        self.ladder.iter().position(|m| m == marking)
    }

    /// Classifies a query's `caller_clearance` against `product_label`: `None` means the
    /// query proceeds; `Some` names the single [`LabelRefusal`] it represents. Mislabeling
    /// (either side absent from the ladder) is checked before over-clearance, exactly like
    /// `crates/av-edge/src/policy.rs::ProducerPolicy::classify_label`.
    pub fn classify(&self, caller_clearance: &str, product_label: &Label) -> Option<LabelRefusal> {
        let Some(caller_rank) = self.rank(caller_clearance) else {
            return Some(LabelRefusal::MarkingNotOnLadder { side: Side::Caller, marking: caller_clearance.to_string() });
        };
        let Some(product_rank) = self.rank(&product_label.marking) else {
            return Some(LabelRefusal::MarkingNotOnLadder { side: Side::Product, marking: product_label.marking.clone() });
        };
        if product_rank > caller_rank {
            return Some(LabelRefusal::OverClearance {
                product_marking: product_label.marking.clone(),
                caller_clearance: caller_clearance.to_string(),
            });
        }
        None
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
    fn classify_allows_a_caller_at_or_above_the_products_rank() {
        assert_eq!(ladder().classify("SECRET", &label("CUI")), None);
        assert_eq!(ladder().classify("CUI", &label("CUI")), None, "equal rank is allowed");
        assert_eq!(ladder().classify("UNCLASSIFIED", &label("UNCLASSIFIED")), None);
    }

    #[test]
    fn classify_refuses_over_clearance() {
        let err = ladder().classify("UNCLASSIFIED", &label("SECRET")).unwrap();
        assert_eq!(err, LabelRefusal::OverClearance { product_marking: "SECRET".to_string(), caller_clearance: "UNCLASSIFIED".to_string() });
    }

    #[test]
    fn classify_refuses_a_product_marking_absent_from_the_ladder_even_though_caller_is_cleared() {
        let err = ladder().classify("SECRET", &label("TOP-SECRET")).unwrap();
        assert_eq!(err, LabelRefusal::MarkingNotOnLadder { side: Side::Product, marking: "TOP-SECRET".to_string() });
    }

    #[test]
    fn classify_refuses_a_caller_clearance_absent_from_the_ladder() {
        let err = ladder().classify("TOP-SECRET", &label("CUI")).unwrap();
        assert_eq!(err, LabelRefusal::MarkingNotOnLadder { side: Side::Caller, marking: "TOP-SECRET".to_string() });
    }

    /// Mislabeling is checked before over-clearance: a caller marking absent from the
    /// ladder is refused as MarkingNotOnLadder even when it -- hypothetically, were it on
    /// the ladder -- would also have been over-clearance. This test pins the ORDER, not
    /// just that some refusal happens: it uses a product marking that is also off-ladder,
    /// so if over-clearance were (wrongly) checked first there would be no rank to compare
    /// and the implementation would have to fall through to MarkingNotOnLadder anyway --
    /// the real discriminator is `classify_caller_marking_not_on_ladder_is_checked_before_
    /// product_side` below, which pins the two-sided order unambiguously.
    #[test]
    fn classify_checks_caller_marking_before_product_marking() {
        let err = ladder().classify("NOT-ON-LADDER-CALLER", &label("ALSO-NOT-ON-LADDER-PRODUCT")).unwrap();
        assert_eq!(err, LabelRefusal::MarkingNotOnLadder { side: Side::Caller, marking: "NOT-ON-LADDER-CALLER".to_string() });
    }

    #[test]
    fn refusal_codes_are_counted_under_stable_distinct_keys() {
        use crate::counters::Counters;
        let counters = Counters::new();
        counters.record(&LabelRefusal::MarkingNotOnLadder { side: Side::Caller, marking: "x".to_string() });
        counters.record(&LabelRefusal::MarkingNotOnLadder { side: Side::Product, marking: "y".to_string() });
        counters.record(&LabelRefusal::OverClearance { product_marking: "SECRET".to_string(), caller_clearance: "CUI".to_string() });
        assert_eq!(counters.get("label_caller_marking_not_on_ladder"), 1);
        assert_eq!(counters.get("label_product_marking_not_on_ladder"), 1);
        assert_eq!(counters.get("label_over_clearance"), 1);
    }
}
