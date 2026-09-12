//! A producer's declared emit label, clearance and staleness budget -- what
//! `crate::chain::ChainVerifier` checks an incoming batch's `Label` and `batch_tai_ns`
//! against.
//!
//! The clearance ladder is deliberately an **explicit, ordered list of markings supplied
//! by configuration** (`ProducerPolicy::clearance_ladder`, rank = index into it), not a
//! hardcoded enum or a numeric level: this platform enforces exactly one handling level
//! per deployment (ADR-004, question 32) but different deployments' customers spell their
//! schemes differently ("CUI" / "CUI//SP-EXPT" / "UNCLASSIFIED" is `Label.marking`'s own
//! doc comment example), so the ladder -- and therefore what counts as "above" a given
//! clearance -- is data a deployment configures, never a fact this crate bakes in. A
//! marking that is not on the ladder at all is never silently let through at whatever
//! rank an absent lookup would default to; it is refused as MISLABELED, exactly like a
//! marking that does not match what the producer declared it emits under.

use crate::pb;

/// What can go wrong constructing a [`ProducerPolicy`]. A policy whose own `clearance`
/// is not on its own `clearance_ladder` is refused at construction, not discovered later
/// as an internal panic the first time a batch happens to need the clearance rank.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("policy's clearance marking {clearance:?} is not on its own clearance_ladder {ladder:?}")]
    ClearanceNotOnLadder { clearance: String, ladder: Vec<String> },
}

/// One producer's declared identity within the label/clearance system: what it says it
/// emits under, how far up the deployment's clearance ladder it is trusted, and how old a
/// batch may be before it is refused as stale.
#[derive(Debug, Clone)]
pub struct ProducerPolicy {
    /// The producer this policy governs (`MeasurementBatch.producer_id`). Informational
    /// for this struct itself -- `ChainVerifier` is what actually matches a policy to an
    /// incoming batch's `producer_id`.
    pub producer_id: String,
    /// The exact marking every batch from this producer must carry (`Label.marking`). A
    /// batch whose label names any other marking is MISLABELED, even if that other
    /// marking is itself a valid, lower-ranked entry on `clearance_ladder` -- a producer
    /// may only ever emit under the one marking it declared, not "anything it is cleared
    /// for".
    pub emit_marking: String,
    /// The exact caveat set every batch from this producer must carry (`Label.caveats`),
    /// compared order-independently (a caveat list is a set, not a sequence). A batch
    /// whose caveats differ at all -- extra, missing, or substituted -- is MISLABELED.
    pub emit_caveats: Vec<String>,
    /// The deployment's ordered clearance ladder: `clearance_ladder[i]` outranks
    /// `clearance_ladder[j]` for every `i > j`. A marking absent from this list has no
    /// rank and is always MISLABELED, regardless of `clearance`.
    pub clearance_ladder: Vec<String>,
    /// This producer's own clearance: the highest marking on `clearance_ladder` it may
    /// emit under. Must itself appear in `clearance_ladder` ([`ProducerPolicy::new`]
    /// enforces this at construction).
    pub clearance: String,
    /// The oldest a batch's `batch_tai_ns` may be, relative to the chain verifier's
    /// injected clock, before it is refused as STALE. Nanoseconds, matching every other
    /// duration-shaped quantity that touches a TAI epoch on this platform.
    pub max_age_ns: i64,
}

impl ProducerPolicy {
    /// Constructs a policy, refusing one whose own `clearance` does not appear on its own
    /// `clearance_ladder` -- a policy like that could never accept anything from its
    /// producer (every lookup of `clearance`'s rank would have nothing to compare
    /// against), which is almost certainly a configuration mistake, not an intentional
    /// "clearance of nothing" policy (a deployment that really means that should simply
    /// never register a policy for that producer at all).
    pub fn new(
        producer_id: impl Into<String>,
        emit_marking: impl Into<String>,
        emit_caveats: Vec<String>,
        clearance_ladder: Vec<String>,
        clearance: impl Into<String>,
        max_age_ns: i64,
    ) -> Result<Self, PolicyError> {
        let clearance = clearance.into();
        if !clearance_ladder.iter().any(|m| m == &clearance) {
            return Err(PolicyError::ClearanceNotOnLadder { clearance, ladder: clearance_ladder });
        }
        Ok(Self { producer_id: producer_id.into(), emit_marking: emit_marking.into(), emit_caveats, clearance_ladder, clearance, max_age_ns })
    }

    /// This policy's own clearance rank (an index into `clearance_ladder`). Always
    /// `Some` for a policy built through [`ProducerPolicy::new`]; `expect`-safe here
    /// because that constructor is the only supported way to build one.
    fn clearance_rank(&self) -> usize {
        self.clearance_ladder.iter().position(|m| m == &self.clearance).expect("ProducerPolicy::new validates clearance is on clearance_ladder")
    }

    /// Classifies `label` against this policy: `None` means it is fine to accept (matches
    /// the declared emit marking and caveat set, and that marking's rank does not exceed
    /// this producer's clearance rank); `Some` names the single defect it represents.
    /// Mislabeling is checked before over-clearance (documented, fixed order within this
    /// one function): a label that already fails to match the declared emit label is
    /// MISLABELED regardless of where its marking happens to rank, since "over
    /// clearance" is only a meaningful diagnosis for a label that *is* otherwise the
    /// producer's own declared label.
    pub fn classify_label(&self, label: &pb::Label) -> Option<pb::BatchRejection> {
        let marking_matches = label.marking == self.emit_marking;
        let caveats_match = caveat_sets_equal(&label.caveats, &self.emit_caveats);
        if !marking_matches || !caveats_match {
            return Some(pb::BatchRejection::Mislabeled);
        }
        let Some(rank) = self.clearance_ladder.iter().position(|m| m == &label.marking) else {
            // The producer's own declared emit_marking is not on the ladder at all --
            // never silently accepted just because it matches what was declared.
            return Some(pb::BatchRejection::Mislabeled);
        };
        if rank > self.clearance_rank() {
            return Some(pb::BatchRejection::OverClearance);
        }
        None
    }

    /// Whether a batch whose own declared epoch is `batch_tai_ns` counts as stale
    /// relative to `now_tai_ns` (the chain verifier's injected clock, never a live read
    /// of any clock -- question 199). A batch from the future (`batch_tai_ns >
    /// now_tai_ns`) is never stale by this definition; clock skew between producer and
    /// verifier is a separate concern this policy does not attempt to bound.
    pub fn is_stale(&self, batch_tai_ns: i64, now_tai_ns: i64) -> bool {
        now_tai_ns.saturating_sub(batch_tai_ns) > self.max_age_ns
    }
}

/// Order-independent equality over two caveat lists -- a caveat set, not a sequence
/// (`ProducerPolicy::emit_caveats`'s own doc comment).
fn caveat_sets_equal(a: &[String], b: &[String]) -> bool {
    let mut a_sorted = a.to_vec();
    let mut b_sorted = b.to_vec();
    a_sorted.sort();
    b_sorted.sort();
    a_sorted == b_sorted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ladder_policy() -> ProducerPolicy {
        ProducerPolicy::new(
            "sim-asset-1",
            "CUI",
            vec!["SP-EXPT".to_string()],
            vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()],
            "CUI",
            5_000_000_000,
        )
        .unwrap()
    }

    #[test]
    fn new_refuses_a_clearance_not_on_its_own_ladder() {
        let err = ProducerPolicy::new("p", "CUI", vec![], vec!["UNCLASSIFIED".to_string()], "SECRET", 0).unwrap_err();
        assert!(matches!(err, PolicyError::ClearanceNotOnLadder { .. }), "{err:?}");
    }

    #[test]
    fn classify_label_accepts_the_exact_declared_label() {
        let policy = ladder_policy();
        let label = pb::Label { marking: "CUI".to_string(), caveats: vec!["SP-EXPT".to_string()] };
        assert_eq!(policy.classify_label(&label), None);
    }

    #[test]
    fn classify_label_is_order_independent_over_caveats() {
        let policy = ProducerPolicy::new(
            "p",
            "CUI",
            vec!["A".to_string(), "B".to_string()],
            vec!["CUI".to_string()],
            "CUI",
            0,
        )
        .unwrap();
        let label = pb::Label { marking: "CUI".to_string(), caveats: vec!["B".to_string(), "A".to_string()] };
        assert_eq!(policy.classify_label(&label), None, "caveat sets must compare order-independently");
    }

    #[test]
    fn classify_label_rejects_a_different_marking_as_mislabeled() {
        let policy = ladder_policy();
        let label = pb::Label { marking: "SECRET".to_string(), caveats: vec!["SP-EXPT".to_string()] };
        assert_eq!(policy.classify_label(&label), Some(pb::BatchRejection::Mislabeled));
    }

    #[test]
    fn classify_label_rejects_a_different_caveat_set_as_mislabeled() {
        let policy = ladder_policy();
        let label = pb::Label { marking: "CUI".to_string(), caveats: vec!["SOME-OTHER-CAVEAT".to_string()] };
        assert_eq!(policy.classify_label(&label), Some(pb::BatchRejection::Mislabeled));
    }

    #[test]
    fn classify_label_rejects_a_marking_not_on_the_ladder_as_mislabeled_even_if_declared() {
        // A policy can be misconfigured to declare an emit_marking that never made it
        // onto the ladder; that must never be silently treated as acceptable just
        // because it matches what was declared.
        let policy = ProducerPolicy::new("p", "NOT-ON-LADDER", vec![], vec!["CUI".to_string()], "CUI", 0).unwrap();
        let label = pb::Label { marking: "NOT-ON-LADDER".to_string(), caveats: vec![] };
        assert_eq!(policy.classify_label(&label), Some(pb::BatchRejection::Mislabeled));
    }

    #[test]
    fn classify_label_rejects_a_matching_but_over_clearance_marking() {
        // The declared emit label itself outranks the producer's own configured
        // clearance -- a misconfiguration classify_label must still catch.
        let policy = ProducerPolicy::new(
            "p",
            "SECRET",
            vec![],
            vec!["UNCLASSIFIED".to_string(), "CUI".to_string(), "SECRET".to_string()],
            "CUI",
            0,
        )
        .unwrap();
        let label = pb::Label { marking: "SECRET".to_string(), caveats: vec![] };
        assert_eq!(policy.classify_label(&label), Some(pb::BatchRejection::OverClearance));
    }

    #[test]
    fn is_stale_uses_the_injected_clock_not_any_live_clock() {
        let policy = ladder_policy();
        assert!(!policy.is_stale(1_000, 1_000 + 4_999_999_999), "just under max_age_ns is not stale");
        assert!(policy.is_stale(1_000, 1_000 + 5_000_000_001), "just over max_age_ns is stale");
        assert!(!policy.is_stale(10_000, 1_000), "a batch from the future is never stale");
    }
}
