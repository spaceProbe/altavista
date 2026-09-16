//! The clearance-ladder convention, adapted to `crate::query::find_assets`'s own need: the
//! set of markings a caller's claimed clearance is entitled to see.
//!
//! **This is an ADAPTER, not a copy.** Before question 218's extraction, this module held
//! its own full reimplementation of the ladder (this crate's own fourth copy of the
//! convention in the workspace, after `av-edge`, `av-gateway` and `av-store`). It now
//! re-exports [`av_label::ClearanceLadder`] directly, so `crate::labels::ClearanceLadder`
//! and `av_catalog::ClearanceLadder` both still resolve. `ClearanceLadder::
//! markings_at_or_below` is already the exact method name and shape
//! `crate::query::find_assets` needs (`Err` side: [`av_label::LabelRefusal`] rather than
//! this crate's own [`crate::error::CatalogError`]) -- so THIS crate's own adapter step is
//! not a wrapper method (naming a wrapper method `markings_at_or_below` here would never be
//! called anyway: an INHERENT method on `ClearanceLadder` itself always wins method
//! resolution over a same-named trait method, so a local extension trait could not shadow
//! it even if written) but the `impl From<av_label::LabelRefusal> for CatalogError` in
//! `crate::error`: `crate::query::find_assets`'s own `ladder.markings_at_or_below(
//! caller_clearance)?` line is UNCHANGED, character for character, and the `?` operator's
//! own automatic `From` conversion is what turns a `LabelRefusal` into this crate's own
//! `CatalogError` at that call site. See `crate::error`'s own doc, at that `From` impl, for
//! exactly which `CatalogError` variant each `LabelRefusal` variant maps to and why.
//!
//! A marking absent from the ladder is refused, never defaulted to a rank (here: the
//! CALLER's own marking -- the subject side of that rule falls out for free, restated in
//! `crate::query`'s own module doc: a stored `marking` that is not itself one of the values
//! in the returned set, because it is not on the ladder at all, can never match `= ANY(...)`
//! under any caller clearance).

pub use av_label::ClearanceLadder;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CatalogError;

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

    /// The raw `av_label::LabelRefusal` this crate's own `?` operator converts via
    /// `CatalogError`'s `From` impl (`crate::error`'s own doc) -- converted explicitly here
    /// (rather than relying on a `?` inside the test itself) so the assertion is against
    /// THIS crate's own `CatalogError::CallerMarkingNotOnLadder`, unchanged in outcome from
    /// before this extraction.
    #[test]
    fn markings_at_or_below_refuses_a_caller_clearance_absent_from_the_ladder() {
        let err: CatalogError = ladder().markings_at_or_below("TOP-SECRET").unwrap_err().into();
        assert!(matches!(&err, CatalogError::CallerMarkingNotOnLadder { marking } if marking == "TOP-SECRET"), "{err:?}");
    }

    #[test]
    fn markings_at_or_below_never_returns_a_marking_above_the_callers_rank() {
        let result = ladder().markings_at_or_below("CUI").unwrap();
        assert!(!result.contains(&"SECRET".to_string()), "{result:?}");
    }
}
