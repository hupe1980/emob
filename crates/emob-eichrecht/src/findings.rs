//! What stood between a session and its invoice, as typed findings an operator
//! page can word — each with the paragraph it rests on.
//!
//! [`crate::Evidence::reasons`] is one English sentence per problem, written for
//! a log and an operator queue. A page that answers *"warum?"* in two languages
//! needs more than the sentence: **which kind** of fault it was — so it can say
//! what would have made the session billable — and **which rule** decided it.
//! Reading either back out of the sentence would be the string-matching this
//! workspace refuses everywhere else, so the kind is decided here, once, beside
//! the problem it classifies.
//!
//! The mapping is total over [`EvidenceProblem`], and a sequence finding this
//! build does not name lands in [`FindingKind::Sequence`] — never in nothing.

use crate::chain::{ChainFinding, Disqualifies};
use crate::error::{EichrechtError, KeyLookupError};
use crate::evidence::{Evidence, EvidenceProblem};

/// Which kind of fault a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum FindingKind {
    /// No key is registered for the signing component at the signing instant.
    NoKey,
    /// The signature did not verify against the registered key.
    Signature,
    /// A record could not be read at all.
    Unreadable,
    /// The meter reported a state or an error flag that may not be billed.
    MeterState,
    /// The clock behind the readings is not one a duration may be billed on.
    Clock,
    /// The billed register is one the format reserves.
    Register,
    /// The readings are not in one energy unit.
    Unit,
    /// Energy moved in both directions.
    Directions,
    /// Cable-loss compensation that cannot be traced.
    CableLoss,
    /// The identification changed or failed during the session.
    Identification,
    /// The records do not form one complete, ordered sequence.
    Sequence,
    /// There was no usable pair of energy readings.
    NoEnergy,
}

impl FindingKind {
    /// The paragraph the finding rests on.
    #[must_use]
    pub const fn citation(self) -> &'static str {
        match self {
            // A value nobody can verify may not be billed — whether the key is
            // missing, the signature fails, or there is nothing to subtract.
            Self::NoKey | Self::Signature | Self::NoEnergy => "[MessEG §33]",
            Self::Unreadable => "[OCMF Tab. 7]",
            Self::MeterState => "[OCMF Tab. 7, EF]",
            Self::Clock => "[OCMF Tab. 19]",
            Self::Register | Self::Directions => "[OCMF Tab. 25]",
            Self::Unit => "[OCMF Tab. 7, RU]",
            Self::CableLoss => "[OCMF Tab. 7, CL]",
            Self::Identification => "[OCMF Tab. 11]",
            Self::Sequence => "[OCMF §Signing and Verification Process]",
        }
    }
}

/// One finding of the evidence chain.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Finding {
    /// Which kind of fault.
    pub kind: FindingKind,
    /// What it takes away.
    pub takes: Disqualifies,
    /// The chain's own sentence, as [`Evidence::reasons`] gives it.
    pub detail: String,
}

impl Evidence {
    /// Every problem with these records as a typed finding, in the order found.
    ///
    /// Empty when the records bill. A finding that takes only the duration —
    /// an unsynchronised clock — is listed too, with `takes` saying so.
    #[must_use]
    pub fn findings(&self) -> Vec<Finding> {
        self.problems.iter().map(finding).collect()
    }
}

fn finding(problem: &EvidenceProblem) -> Finding {
    let (kind, takes) = match problem {
        EvidenceProblem::Signature { error, .. } => (signature_kind(error), Disqualifies::Both),
        EvidenceProblem::Chain(found) => (chain_kind(found), found.disqualifies()),
    };
    Finding {
        kind,
        takes,
        detail: problem.to_string(),
    }
}

fn signature_kind(error: &EichrechtError) -> FindingKind {
    match error {
        EichrechtError::Key(KeyLookupError::Unreadable(_)) | EichrechtError::Parse(_) => {
            FindingKind::Unreadable
        }
        EichrechtError::Key(_) => FindingKind::NoKey,
        // A signature that does not check out, and anything a later release of
        // this error adds: the record is not evidence.
        _ => FindingKind::Signature,
    }
}

fn chain_kind(found: &ChainFinding) -> FindingKind {
    use ocmf::session::Finding as Seq;
    match found {
        ChainFinding::Sequence(sequence) => match sequence {
            Seq::MeterNotOk { .. } | Seq::ErrorFlagged { .. } | Seq::TransactionFaulted { .. } => {
                FindingKind::MeterState
            }
            Seq::ClockNotSynchronised { .. } | Seq::TimeWentBackwards => FindingKind::Clock,
            Seq::IdentificationError { .. } => FindingKind::Identification,
            _ => FindingKind::Sequence,
        },
        ChainFinding::ReservedRegister { .. } => FindingKind::Register,
        ChainFinding::NotAnEnergyUnit { .. } | ChainFinding::UnitChanged { .. } => {
            FindingKind::Unit
        }
        ChainFinding::MixedDirections { .. } => FindingKind::Directions,
        ChainFinding::LossOnNonAccumulationRegister { .. }
        | ChainFinding::LossNotResetAtBegin { .. } => FindingKind::CableLoss,
        ChainFinding::IdentificationChanged { .. } => FindingKind::Identification,
        ChainFinding::UnknownErrorFlag { .. } => FindingKind::MeterState,
        ChainFinding::NoBillableEnergy => FindingKind::NoEnergy,
        // `ChainFinding` is `#[non_exhaustive]` to callers, not to this crate,
        // but a new variant must still land somewhere a page can word.
        #[allow(unreachable_patterns)]
        _ => FindingKind::Sequence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_cites_a_paragraph_in_the_bracketed_form() {
        use FindingKind::*;
        for kind in [
            NoKey,
            Signature,
            Unreadable,
            MeterState,
            Clock,
            Register,
            Unit,
            Directions,
            CableLoss,
            Identification,
            Sequence,
            NoEnergy,
        ] {
            let cite = kind.citation();
            assert!(
                cite.starts_with('[') && cite.ends_with(']'),
                "{kind:?}: {cite}"
            );
        }
    }

    #[test]
    fn a_chain_with_nothing_to_subtract_is_no_energy_and_takes_the_energy() {
        let found = finding(&EvidenceProblem::Chain(ChainFinding::NoBillableEnergy));
        assert_eq!(found.kind, FindingKind::NoEnergy);
        assert_eq!(found.takes, Disqualifies::Energy);
        assert_eq!(found.detail, "no usable pair of energy readings");
    }
}
