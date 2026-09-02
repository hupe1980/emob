//! What a claim refuses, and why.

use thiserror::Error;

/// Everything that stops a kilowatt-hour reaching a notification.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ThgError {
    /// The obligation year is before the first year the counting factor is
    /// stated for `[38k §5(3)]`.
    #[error(
        "obligation year {year} has no counting factor: `[38k §5(3)]` states one from {first} onwards"
    )]
    YearNotCounted {
        /// The year asked about.
        year: i32,
        /// The first year the paragraph states a factor for.
        first: i32,
    },

    /// The point is not publicly accessible, so `[38k §6]` never engages.
    #[error("{evse_id} is not publicly accessible: `[38k §6]` counts only public points")]
    NotPublic {
        /// The point.
        evse_id: String,
    },

    /// The point fails one of `[38k §6(3)]`'s four conditions.
    #[error("{evse_id} is not eligible: {remedy}")]
    NotEligible {
        /// The point.
        evse_id: String,
        /// What the calendar says to do about it.
        remedy: String,
    },

    /// The withdrawal did not happen in the German electricity-tax territory
    /// `[38k §5(1)]`.
    #[error(
        "{evse_id} is in {country}: `[38k §5(1)]` counts only withdrawals in the German electricity-tax territory"
    )]
    OutsideTaxTerritory {
        /// The point.
        evse_id: String,
        /// The country its identifier names.
        country: String,
    },

    /// The third party has no `[38k §5(2)]` agreement with the point's
    /// operator.
    #[error(
        "{third_party} has no `[38k §5(2)]` agreement with operator {operator}, which operates {evse_id}"
    )]
    NoAgreement {
        /// The third party filing the notification.
        third_party: String,
        /// The operator the identifier names.
        operator: String,
        /// The point.
        evse_id: String,
    },

    /// A record carries energy no meter signed.
    ///
    /// `[38k §6(3) Nr. 2]` requires the energetic quantity to be determined in
    /// conformity with the measuring and calibration law. A record with no
    /// evidence behind it cannot show that, and a claim that included it would
    /// be one the third party's own declaration `[38k §6(4)]` contradicts.
    #[error("{cdr} carries no signed evidence: `[38k §6(3) Nr. 2]` cannot be shown for it")]
    Unmeasured {
        /// The record.
        cdr: String,
    },

    /// A renewable basis was claimed without the whole of `[38k §5(5)]`.
    ///
    /// The paragraph's own remedy is the grid average of `[38k §5(4)]`, so
    /// this refusal is the fallback stated rather than performed silently.
    #[error(
        "the renewable basis is not available ({missing}): `[38k §5(5)]` falls back to `[38k §5(4)]`"
    )]
    ProofIncomplete {
        /// Which of the paragraph's conditions is not met.
        missing: String,
    },

    /// A renewable source that is not countable in the obligation year.
    #[error(
        "{energy_source} counts from obligation year {from} `[38k §5(5) S. 1 Nr. 1]`, and this claim is for {year}"
    )]
    SourceNotYetCountable {
        /// The source, in the Verordnung's own word for it.
        energy_source: &'static str,
        /// The first year it counts.
        from: i32,
        /// The claim's year.
        year: i32,
    },

    /// An emissions value that cannot be one.
    #[error("{what} must not be negative, got {value}")]
    Negative {
        /// Which value.
        what: &'static str,
        /// What was supplied.
        value: String,
    },

    /// A notification with nothing in it.
    #[error("`[38k §8(1)]` has nothing to report: no point contributed countable energy")]
    NothingToReport,
}
