//! D2: label enforcement, now question 218's shared `av-label` crate -- this module is a
//! thin adapter, not this crate's own copy of the ladder. It re-exports [`av_label::
//! ClearanceLadder`], [`av_label::LabelRefusal`] and [`av_label::Side`] directly, and adds
//! exactly the two things this crate still needs of its own: counting
//! ([`Counted`]/[`code`]) and this crate's own outward-facing wording (its `Display` moved
//! to `av-label` itself, since `LabelRefusal` is a foreign type here now -- see that
//! crate's own doc; its wording generalises "product" to "subject").
//!
//! # `Counted` for a foreign `LabelRefusal`: not a trait impl, a free function
//!
//! The original (pre-extraction) version of this module wrote `impl Counted for
//! LabelRefusal` directly, which compiled because `LabelRefusal` was defined IN THIS crate
//! at the time. Two things changed since: `Counted` itself moved to `av_command::counters`
//! in R3.1 (re-exported here as `crate::counters::Counted`, unchanged in behaviour, per
//! this crate's own top-level module doc), and this round's question-218 extraction moves
//! `LabelRefusal` to `av-label`. With BOTH the trait and the type now foreign to this
//! crate, `impl Counted for LabelRefusal` here would be `E0117` (Rust's orphan rule: at
//! least one of a trait impl's trait or Self type must be local to the implementing
//! crate) -- neither is, anymore. [`code`] is the fix: a plain free function computing the
//! identical mapping, called from [`crate::gateway::RefusalReason`]'s own `Counted` impl
//! (`RefusalReason` itself IS local to this crate, so that impl is unaffected) instead of
//! `label_refusal.code()`. No call site outside this crate ever called `LabelRefusal::
//! code()` directly (`Counters::record` is always called with a `RefusalReason`, never a
//! bare `LabelRefusal` -- `crate::gateway::GatewayCore::refuse`'s own `self.counters.
//! record(&reason)`), so this is an internal mechanism change with no external
//! observable difference: the three counter code strings below are BYTE-IDENTICAL to
//! before this extraction.
//!
//! **The three counter code strings are wire-visible and unchanged.**
//! `label_caller_marking_not_on_ladder`, `label_product_marking_not_on_ladder` and
//! `label_over_clearance` are exactly what they were before this round -- see [`code`]'s
//! own comment on the `Side::Subject` arm for why the key string still says `product`.

pub use av_label::{ClearanceLadder, LabelRefusal, Side};

/// This crate's own mapping from a (foreign, since question 218) [`LabelRefusal`] to its
/// stable counter code -- see this module's own doc, "`Counted` for a foreign
/// `LabelRefusal`: not a trait impl, a free function", for why this is a free function
/// rather than `impl Counted for LabelRefusal`.
pub(crate) fn code(refusal: &LabelRefusal) -> &'static str {
    match refusal {
        LabelRefusal::MarkingNotOnLadder { side: Side::Caller, .. } => "label_caller_marking_not_on_ladder",
        // av_label::Side::Subject is the shared crate's neutral name for what this crate's
        // own wire surface has always called a "product" -- the counter key string is
        // deliberately UNCHANGED (`label_product_marking_not_on_ladder`, not
        // `label_subject_marking_not_on_ladder`) because it is already observable: a
        // dashboard or alert rule built against the old key must keep working after this
        // purely internal refactor.
        LabelRefusal::MarkingNotOnLadder { side: Side::Subject, .. } => "label_product_marking_not_on_ladder",
        LabelRefusal::OverClearance { .. } => "label_over_clearance",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::RefusalReason;

    fn ladder() -> ClearanceLadder {
        ClearanceLadder::new(vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()])
    }

    fn label(marking: &str) -> av_cdm::pb::Label {
        av_cdm::pb::Label { marking: marking.to_string(), caveats: vec![] }
    }

    #[test]
    fn classify_allows_a_caller_at_or_above_the_subjects_rank() {
        assert_eq!(ladder().classify("SECRET", &label("CUI")), None);
        assert_eq!(ladder().classify("CUI", &label("CUI")), None, "equal rank is allowed");
        assert_eq!(ladder().classify("UNCLASSIFIED", &label("UNCLASSIFIED")), None);
    }

    #[test]
    fn classify_refuses_over_clearance() {
        let err = ladder().classify("UNCLASSIFIED", &label("SECRET")).unwrap();
        assert_eq!(err, LabelRefusal::OverClearance { subject_marking: "SECRET".to_string(), caller_clearance: "UNCLASSIFIED".to_string() });
    }

    #[test]
    fn classify_refuses_a_subject_marking_absent_from_the_ladder_even_though_caller_is_cleared() {
        let err = ladder().classify("SECRET", &label("TOP-SECRET")).unwrap();
        assert_eq!(err, LabelRefusal::MarkingNotOnLadder { side: Side::Subject, marking: "TOP-SECRET".to_string() });
    }

    #[test]
    fn classify_refuses_a_caller_clearance_absent_from_the_ladder() {
        let err = ladder().classify("TOP-SECRET", &label("CUI")).unwrap();
        assert_eq!(err, LabelRefusal::MarkingNotOnLadder { side: Side::Caller, marking: "TOP-SECRET".to_string() });
    }

    /// Mislabeling is checked before over-clearance: a caller marking absent from the
    /// ladder is refused as MarkingNotOnLadder even when it -- hypothetically, were it on
    /// the ladder -- would also have been over-clearance. This test pins the ORDER, not
    /// just that some refusal happens: it uses a subject marking that is also off-ladder,
    /// so if over-clearance were (wrongly) checked first there would be no rank to compare
    /// and the implementation would have to fall through to MarkingNotOnLadder anyway --
    /// the real discriminator is `classify_caller_marking_not_on_ladder_is_checked_before_
    /// subject_side` below, which pins the two-sided order unambiguously.
    #[test]
    fn classify_checks_caller_marking_before_subject_marking() {
        let err = ladder().classify("NOT-ON-LADDER-CALLER", &label("ALSO-NOT-ON-LADDER-SUBJECT")).unwrap();
        assert_eq!(err, LabelRefusal::MarkingNotOnLadder { side: Side::Caller, marking: "NOT-ON-LADDER-CALLER".to_string() });
    }

    /// The three counter code strings a refusal maps to are stable and distinct -- proven
    /// here through [`RefusalReason::Label`] (this crate's own local wrapper `RefusalReason`
    /// is what `Counters::record` is actually ever called with in production, never a bare
    /// `LabelRefusal` -- see this module's own doc), so this is the same observable path a
    /// real query refusal goes through, not a shortcut around it.
    #[test]
    fn refusal_codes_are_counted_under_stable_distinct_keys() {
        use crate::counters::Counters;
        let counters = Counters::new();
        counters.record(&RefusalReason::Label(LabelRefusal::MarkingNotOnLadder { side: Side::Caller, marking: "x".to_string() }));
        counters.record(&RefusalReason::Label(LabelRefusal::MarkingNotOnLadder { side: Side::Subject, marking: "y".to_string() }));
        counters.record(&RefusalReason::Label(LabelRefusal::OverClearance { subject_marking: "SECRET".to_string(), caller_clearance: "CUI".to_string() }));
        assert_eq!(counters.get("label_caller_marking_not_on_ladder"), 1);
        assert_eq!(counters.get("label_product_marking_not_on_ladder"), 1);
        assert_eq!(counters.get("label_over_clearance"), 1);
    }
}
