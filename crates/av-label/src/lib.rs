//! Question 218: **the one shared home for this workspace's clearance-ladder convention.**
//!
//! Before this crate existed, the convention lived as four separate, hand-copied
//! implementations: `crates/av-edge/src/policy.rs::ProducerPolicy` (which originated it),
//! `crates/av-gateway/src/labels.rs::ClearanceLadder`, `crates/av-store/src/labels.rs::
//! ClearanceLadder` and `crates/av-catalog/src/labels.rs::ClearanceLadder` (the second,
//! third and fourth, each one's own module doc naming exactly why it could not simply
//! depend on an earlier copy -- wrong dependency direction, or a track boundary neither
//! could cross). Four copies is a divergence waiting to happen (question 218's own words):
//! a fix or a review finding landing in one copy and not the other three is not a
//! hypothetical, it is what inevitably happens to hand-maintained duplicates over enough
//! rounds. This crate is where the convention now lives, ONCE: `av-store`, `av-catalog` and
//! `av-gateway` all adopt it this round (each crate's own `src/labels.rs` becomes a thin
//! adapter over the [`ClearanceLadder`] this module defines, documented at each adapter's
//! own module doc). `av-edge`'s `ProducerPolicy` is deliberately left as its own, fourth
//! copy this round -- see "Why `av-edge` is not adopted this round" below.
//!
//! # The convention itself
//!
//! A deployment's clearance scheme is an **explicit, ordered list of markings supplied by
//! configuration** ([`ClearanceLadder::new`]'s `ladder` argument; rank = index into it),
//! never a hardcoded enum and never a numeric level. This platform enforces exactly one
//! handling level per deployment (ADR-004, question 32), but different deployments' own
//! customers spell their schemes differently (`Label.marking`'s own doc comment example:
//! `"CUI"` / `"CUI//SP-EXPT"` / `"UNCLASSIFIED"`), so the ladder -- and therefore what
//! counts as "above" a given clearance -- is data a deployment configures, never a fact
//! this crate bakes in.
//!
//! **A marking absent from the ladder is its own refusal, never a default rank.** Neither
//! [`ClearanceLadder::rank`] nor anything built on it ever treats "not found" as rank 0 (or
//! any other rank) and lets a comparison proceed on that assumption -- an unranked marking
//! is refused outright, as [`LabelRefusal::MarkingNotOnLadder`], on whichever side of a
//! comparison it appeared. [`ClearanceLadder::classify`] checks the CALLER's side before
//! the SUBJECT's side, then over-clearance, in that fixed order -- every adopting crate's
//! own tests (and this crate's own, below) pin that order, not merely that some refusal
//! results.
//!
//! # `Label.caveats` is deliberately not compared here
//!
//! (Carried over from `av-gateway`'s own original `src/labels.rs` module doc, the
//! considered reasoning for why this is still correct in the shared crate.) `av-edge`'s
//! `ProducerPolicy::classify_label` checks caveats because it is verifying a producer did
//! not lie about the ONE label it itself declared it emits under (`emit_caveats`) -- there
//! is no equivalent "declared vs. actual" pair in [`ClearanceLadder::classify`]'s own
//! comparison (a caller's clearance is not itself a `Label` with its own caveat set; a
//! subject's label is compared against a bare clearance marking, not against another
//! `Label`). A caveat-aware clearance model is a real possibility for a future deployment,
//! but it is not this crate's convention to invent one without the manager naming it --
//! this module's own ladder rank is the whole of the convention this crate carries.
//!
//! # Why `av-edge` is not adopted this round
//!
//! `crates/av-edge/src/policy.rs::ProducerPolicy` is the fourth copy of this convention,
//! and it is **deliberately left alone this round**: `av-edge` is off this heavy track
//! entirely (question 218, and every adopting crate's own prior module doc already named
//! this boundary), so this extraction does not touch it and nothing in this workspace is
//! made to depend on it. The P5 team adopts `av-label` there themselves, the next time they
//! open `policy.rs` for their own reasons -- not as a side effect of this crate's own
//! creation.
//!
//! # `Side::Subject` is the neutral name
//!
//! [`Side::Subject`] is what `av-gateway`'s own copy called `Product` and `av-store`'s own
//! copy called `Object` -- this shared type carries no domain word from any one of its
//! three consumers. Each adopting crate keeps its OWN outward-facing spelling (a gateway
//! still reports `product`; a store still reports `object`) by mapping at its own adapter
//! boundary; see each crate's own `src/labels.rs` for exactly where that mapping happens.

use av_cdm::pb::Label;

/// Which side of a [`ClearanceLadder::classify`] (or, for the caller side alone,
/// [`ClearanceLadder::markings_at_or_below`]) comparison a [`LabelRefusal::
/// MarkingNotOnLadder`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The calling principal's own claimed clearance.
    Caller,
    /// The thing the caller's clearance is being checked against -- a run product's own
    /// label, a stored object's own label, a catalog row's own marking, depending on which
    /// adopting crate is asking. Never spelled with a domain word here (see this module's
    /// own doc, "`Side::Subject` is the neutral name").
    Subject,
}

/// Why a [`ClearanceLadder`] comparison refused. Every variant names the single defect it
/// represents -- see this module's own doc for the two-sided "absent from the ladder" rule
/// and the "caller checked before subject" order.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LabelRefusal {
    /// `marking` (named by `side`) does not appear anywhere on this deployment's
    /// [`ClearanceLadder`]. Never defaulted to a rank; refused outright.
    #[error("{side:?} marking {marking:?} is not on this deployment's clearance ladder -- refused, never defaulted to a rank")]
    MarkingNotOnLadder { side: Side, marking: String },
    /// Both markings are on the ladder, but the subject's own rank outranks the caller's
    /// claimed clearance rank.
    #[error("subject label {subject_marking:?} outranks caller clearance {caller_clearance:?} on this deployment's clearance ladder")]
    OverClearance { subject_marking: String, caller_clearance: String },
}

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

    /// `marking`'s rank (an index into this ladder), or `None` if `marking` is not on it
    /// at all. `pub`, not `pub(crate)`: every adopting crate reuses this SAME ranking
    /// function -- `av-gateway`'s own `auth.rs::GroupClearanceMap::clearance_for` ranks a
    /// verified token's mapped clearance markings through it directly -- so there is
    /// exactly one ranking function in this whole workspace, never a second,
    /// independently-typed reimplementation anywhere that needs one.
    pub fn rank(&self, marking: &str) -> Option<usize> {
        self.ladder.iter().position(|m| m == marking)
    }

    /// Every marking this ladder configures, in ladder order (lowest rank first).
    pub fn markings(&self) -> &[String] {
        &self.ladder
    }

    /// Classifies `subject_label` against `caller_clearance`: `None` means the comparison
    /// passes (the subject's rank does not exceed the caller's); `Some` names the single
    /// [`LabelRefusal`] it represents. The caller's side is checked before the subject's
    /// side, then over-clearance -- a fixed order, not merely "some refusal happens" (this
    /// module's own doc, and every adopting crate's own tests, pin it).
    pub fn classify(&self, caller_clearance: &str, subject_label: &Label) -> Option<LabelRefusal> {
        let Some(caller_rank) = self.rank(caller_clearance) else {
            return Some(LabelRefusal::MarkingNotOnLadder { side: Side::Caller, marking: caller_clearance.to_string() });
        };
        let Some(subject_rank) = self.rank(&subject_label.marking) else {
            return Some(LabelRefusal::MarkingNotOnLadder { side: Side::Subject, marking: subject_label.marking.clone() });
        };
        if subject_rank > caller_rank {
            return Some(LabelRefusal::OverClearance {
                subject_marking: subject_label.marking.clone(),
                caller_clearance: caller_clearance.to_string(),
            });
        }
        None
    }

    /// Every marking at or below `caller_clearance`'s own rank, INCLUDING `caller_clearance`
    /// itself, in ladder order. `Err(LabelRefusal::MarkingNotOnLadder { side: Side::Caller,
    /// .. })` if `caller_clearance` itself is not on this ladder -- refused before a caller
    /// gets back any set at all. The returned set is drawn from the ladder itself (a
    /// contiguous prefix of it), so a marking that is not on the ladder can never appear in
    /// it, no matter how highly cleared the caller is -- an off-ladder SUBJECT marking is
    /// never something this function can accidentally include, because it never looks at
    /// subject markings at all, only at the ladder's own configured list.
    pub fn markings_at_or_below(&self, caller_clearance: &str) -> Result<Vec<String>, LabelRefusal> {
        let rank = self.rank(caller_clearance).ok_or_else(|| LabelRefusal::MarkingNotOnLadder { side: Side::Caller, marking: caller_clearance.to_string() })?;
        Ok(self.ladder[..=rank].to_vec())
    }
}

/// H4/P0 (`docs/open-questions.md` question 218, round 2): `group -> clearance marking`,
/// deployment-configured, moved here unchanged from `crates/av-gateway/src/auth.rs` (its
/// original, and until now only, home). `av-tiles` needs the identical group-to-clearance
/// mapping `av-gateway` already has -- a tile gateway deriving a caller's clearance from a
/// verified token's groups is exactly the same problem `av-gateway`'s own `AuthContext`
/// solved, and a second, independently-typed copy of `group -> marking` would be precisely
/// the divergence this crate's own module doc (see "Question 218") already warns against.
/// `av-gateway::auth` re-exports [`GroupClearanceMap`] and [`GroupClearanceOutcome`] under
/// their original path (`pub use av_label::{GroupClearanceMap, GroupClearanceOutcome};`)
/// so every existing call site there -- production and test -- keeps compiling unchanged,
/// and every counter code string that depends on the *outcome* of a `clearance_for` call
/// (via `av-gateway`'s own local `AuthRefusal`, which is built from these outcomes but was
/// not itself moved) is byte-identical to before this move.
///
/// A thin `BTreeMap` wrapper, not a second [`ClearanceLadder`]: this map does not itself
/// rank a subject's label against a clearance (that is still entirely [`ClearanceLadder::
/// classify`]'s own job); [`GroupClearanceMap::clearance_for`] ranks a *principal's own*
/// mapped markings against each other only far enough to pick the single highest one,
/// reusing [`ClearanceLadder::rank`] to do it -- never a second, independent ranking
/// implementation.
#[derive(Debug, Clone, Default)]
pub struct GroupClearanceMap {
    by_group: std::collections::BTreeMap<String, String>,
}

/// The result of [`GroupClearanceMap::clearance_for`] -- see that method's own doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupClearanceOutcome {
    /// The highest-ranked (on this deployment's [`ClearanceLadder`]) marking among every one
    /// of the principal's groups this map lists.
    Marking(String),
    /// None of the principal's groups appear in this map at all.
    NoneMapped,
    /// At least one of the principal's mapped groups names a marking absent from this
    /// deployment's ladder -- a misconfiguration, never silently ranked as 0 and never
    /// silently skipped over in favor of a lower mapped marking that IS on the ladder.
    NotOnLadder(String),
}

impl GroupClearanceMap {
    pub fn new(by_group: std::collections::BTreeMap<String, String>) -> Self {
        Self { by_group }
    }

    /// Ranks every one of `groups` that this map lists against `ladder` and returns the
    /// highest-ranked marking -- order-independent: the same set of mapped markings always
    /// yields the same [`GroupClearanceOutcome::Marking`] regardless of the order `groups`
    /// lists them in. A group absent from this map is simply skipped (it asserts no
    /// clearance); a group whose mapped marking is absent from `ladder` short-circuits the
    /// whole call to [`GroupClearanceOutcome::NotOnLadder`] immediately -- never silently
    /// dropped in favor of a lower, on-ladder marking found elsewhere in `groups`.
    /// [`GroupClearanceOutcome::NoneMapped`] only when no group in `groups` is in this map
    /// at all.
    pub fn clearance_for(&self, groups: &[String], ladder: &ClearanceLadder) -> GroupClearanceOutcome {
        let mut best: Option<(usize, &str)> = None;
        for g in groups {
            let Some(marking) = self.by_group.get(g) else { continue };
            let Some(rank) = ladder.rank(marking) else {
                return GroupClearanceOutcome::NotOnLadder(marking.clone());
            };
            if best.map(|(best_rank, _)| rank > best_rank).unwrap_or(true) {
                best = Some((rank, marking.as_str()));
            }
        }
        match best {
            Some((_, marking)) => GroupClearanceOutcome::Marking(marking.to_string()),
            None => GroupClearanceOutcome::NoneMapped,
        }
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

    // -- classify: union of av-gateway's and av-store's own former copies ------------------

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
    fn classify_refuses_a_subject_marking_absent_from_the_ladder_even_for_a_fully_cleared_caller() {
        // "Fully cleared" here means "cleared to this ladder's own top rank" -- the off-
        // ladder subject marking is refused regardless, because it has no rank to compare
        // at all, never because the caller's own clearance was insufficient.
        let err = ladder().classify("SECRET", &label("TOP-SECRET")).unwrap();
        assert_eq!(err, LabelRefusal::MarkingNotOnLadder { side: Side::Subject, marking: "TOP-SECRET".to_string() });
    }

    #[test]
    fn classify_refuses_a_caller_clearance_absent_from_the_ladder() {
        let err = ladder().classify("TOP-SECRET", &label("CUI")).unwrap();
        assert_eq!(err, LabelRefusal::MarkingNotOnLadder { side: Side::Caller, marking: "TOP-SECRET".to_string() });
    }

    /// Pins the ORDER (caller checked before subject), not merely that some refusal
    /// happens: both sides are off-ladder here, so only checking the caller side first can
    /// produce this specific refusal.
    #[test]
    fn classify_checks_caller_marking_before_subject_marking() {
        let err = ladder().classify("NOT-ON-LADDER-CALLER", &label("ALSO-NOT-ON-LADDER-SUBJECT")).unwrap();
        assert_eq!(err, LabelRefusal::MarkingNotOnLadder { side: Side::Caller, marking: "NOT-ON-LADDER-CALLER".to_string() });
    }

    // -- markings_at_or_below: av-catalog's own former copy ---------------------------------

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
        assert_eq!(err, LabelRefusal::MarkingNotOnLadder { side: Side::Caller, marking: "TOP-SECRET".to_string() });
    }

    #[test]
    fn markings_at_or_below_never_returns_a_marking_above_the_callers_rank() {
        let result = ladder().markings_at_or_below("CUI").unwrap();
        assert!(!result.contains(&"SECRET".to_string()), "{result:?}");
    }

    // -- rank/markings: this crate's own direct coverage of the two small accessors --------

    #[test]
    fn rank_is_none_for_a_marking_absent_from_the_ladder() {
        assert_eq!(ladder().rank("TOP-SECRET"), None);
    }

    #[test]
    fn rank_is_the_index_into_the_configured_ladder() {
        assert_eq!(ladder().rank("UNCLASSIFIED"), Some(0));
        assert_eq!(ladder().rank("CUI"), Some(1));
        assert_eq!(ladder().rank("SECRET"), Some(2));
    }

    #[test]
    fn markings_returns_the_whole_configured_ladder_in_order() {
        assert_eq!(ladder().markings(), &["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()]);
    }

    // -- GroupClearanceMap: moved here from crates/av-gateway/src/auth.rs (question 218,
    // round 2, P0) -- direct unit coverage of clearance_for at the layer it now lives at.
    // av-gateway's own auth.rs tests continue to exercise the identical logic through
    // AuthContext::authenticate_query (an integration-level test, not a duplicate of these).

    fn clearance_map(entries: &[(&str, &str)]) -> GroupClearanceMap {
        let mut m = std::collections::BTreeMap::new();
        for (group, marking) in entries {
            m.insert(group.to_string(), marking.to_string());
        }
        GroupClearanceMap::new(m)
    }

    #[test]
    fn clearance_for_a_group_not_in_the_map_at_all_is_none_mapped() {
        let map = clearance_map(&[("operators", "CUI")]);
        assert_eq!(map.clearance_for(&["guests".to_string()], &ladder()), GroupClearanceOutcome::NoneMapped);
    }

    #[test]
    fn clearance_for_a_single_mapped_group_returns_its_marking() {
        let map = clearance_map(&[("operators", "CUI")]);
        assert_eq!(map.clearance_for(&["operators".to_string()], &ladder()), GroupClearanceOutcome::Marking("CUI".to_string()));
    }

    /// Order-independence, pinned directly at this layer: the same set of mapped markings
    /// yields the same highest-ranked result regardless of claim order.
    #[test]
    fn clearance_for_the_same_two_groups_in_either_order_yields_the_same_higher_marking() {
        let map = clearance_map(&[("operators", "CUI"), ("safety-officers", "SECRET")]);
        let high_first = map.clearance_for(&["safety-officers".to_string(), "operators".to_string()], &ladder());
        let low_first = map.clearance_for(&["operators".to_string(), "safety-officers".to_string()], &ladder());
        assert_eq!(high_first, GroupClearanceOutcome::Marking("SECRET".to_string()));
        assert_eq!(low_first, GroupClearanceOutcome::Marking("SECRET".to_string()));
    }

    /// A mapped marking absent from the ladder short-circuits to NotOnLadder immediately --
    /// never silently skipped over in favor of a lower, on-ladder marking mapped elsewhere.
    #[test]
    fn clearance_for_an_off_ladder_marking_short_circuits_even_with_a_lower_on_ladder_marking_present() {
        let map = clearance_map(&[("operators", "CUI"), ("misconfigured-group", "TOP-SECRET")]);
        let outcome = map.clearance_for(&["operators".to_string(), "misconfigured-group".to_string()], &ladder());
        assert_eq!(outcome, GroupClearanceOutcome::NotOnLadder("TOP-SECRET".to_string()));
    }

    #[test]
    fn clearance_for_an_empty_groups_list_is_none_mapped() {
        let map = clearance_map(&[("operators", "CUI")]);
        assert_eq!(map.clearance_for(&[], &ladder()), GroupClearanceOutcome::NoneMapped);
    }

    #[test]
    fn clearance_for_a_default_empty_map_is_always_none_mapped() {
        let map = GroupClearanceMap::default();
        assert_eq!(map.clearance_for(&["anything".to_string()], &ladder()), GroupClearanceOutcome::NoneMapped);
    }
}
