//! The clearance-ladder convention, adapted to this crate's own comparison: a reading
//! principal's claimed clearance against the object's own stored [`Label`].
//!
//! **This is an ADAPTER, not a copy.** Before question 218's extraction, this module held
//! its own full reimplementation of the ladder (this crate's own third copy of the
//! convention in the workspace, after `av-edge` and `av-gateway`). It now re-exports
//! [`av_label::ClearanceLadder`] directly -- so `crate::labels::ClearanceLadder` and
//! `av_store::ClearanceLadder` both still resolve, and `src/client.rs`'s own
//! `use crate::labels::ClearanceLadder;` needed no change at all -- and adds exactly one
//! thing this crate still needs of its own: [`AuthorizeRead::authorize_read`], mapping the
//! shared crate's [`av_label::LabelRefusal`] onto this crate's own [`StoreError`] variants,
//! whose names and `#[error(...)]` messages predate this extraction and are unchanged by
//! it (`crate::error`'s own `StoreError::MarkingNotOnLadder`/`StoreError::OverClearance`,
//! with THIS crate's own `Side::Caller`/`Side::Object` -- `av_label::Side::Subject` maps to
//! `Side::Object` here, this crate's own outward-facing spelling for "the thing being
//! checked against the caller").
//!
//! Mislabeling (either side absent from the ladder) is checked before over-clearance, and
//! the caller's own side is checked before the object's -- both exactly
//! [`av_label::ClearanceLadder::classify`]'s own documented, fixed order, unchanged by this
//! adapter.

use av_cdm::pb::Label;
pub use av_label::ClearanceLadder;

use crate::error::{Side, StoreError};

/// This crate's own entry point onto the shared [`ClearanceLadder`] -- an extension trait,
/// not a wrapper type, so `ladder.authorize_read(caller_clearance, label)` at
/// `src/client.rs`'s own call site stays character for character the same call it always
/// was. (A same-named INHERENT method on `ClearanceLadder` itself would always win method
/// resolution over a trait method of the same name; this trait's own name,
/// `authorize_read`, does not collide with any of `av_label::ClearanceLadder`'s own
/// methods -- `rank`/`markings`/`classify`/`markings_at_or_below` -- so there is nothing to
/// shadow here.)
pub trait AuthorizeRead {
    /// Refuses a read of an object labelled `object_label` by a principal claiming
    /// `caller_clearance`, unless `caller_clearance`'s rank is at or above `object_label`'s
    /// rank on this ladder. See this module's own doc for the refusal order and the
    /// `av_label::LabelRefusal` -> `StoreError` mapping.
    fn authorize_read(&self, caller_clearance: &str, object_label: &Label) -> Result<(), StoreError>;
}

impl AuthorizeRead for ClearanceLadder {
    fn authorize_read(&self, caller_clearance: &str, object_label: &Label) -> Result<(), StoreError> {
        match self.classify(caller_clearance, object_label) {
            None => Ok(()),
            Some(av_label::LabelRefusal::MarkingNotOnLadder { side: av_label::Side::Caller, marking }) => {
                Err(StoreError::MarkingNotOnLadder { side: Side::Caller, marking })
            }
            Some(av_label::LabelRefusal::MarkingNotOnLadder { side: av_label::Side::Subject, marking }) => {
                // av_label::Side::Subject is the shared crate's neutral name; this crate's
                // own outward-facing spelling for the same fact is `Side::Object` (this
                // module's own doc explains why) -- the StoreError variant name and message
                // are unchanged by this extraction.
                Err(StoreError::MarkingNotOnLadder { side: Side::Object, marking })
            }
            Some(av_label::LabelRefusal::OverClearance { subject_marking, caller_clearance }) => {
                Err(StoreError::OverClearance { object_marking: subject_marking, caller_clearance })
            }
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

    /// Pins the order (caller checked before object): both sides are off-ladder, so only
    /// checking the caller side first can produce this specific refusal.
    #[test]
    fn authorize_read_checks_caller_marking_before_object_marking() {
        let err = ladder().authorize_read("NOT-ON-LADDER-CALLER", &label("ALSO-NOT-ON-LADDER-OBJECT")).unwrap_err();
        let repr = format!("{err:?}");
        assert!(matches!(err, StoreError::MarkingNotOnLadder { side: Side::Caller, marking } if marking == "NOT-ON-LADDER-CALLER"), "{repr}");
    }
}
