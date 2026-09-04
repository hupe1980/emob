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
    ///
    /// Which is a refusal by **this** crate rather than a statement that the
    /// energy is worthless: `[38k §7]` is the route for a non-public point, and
    /// it is a different claim by a different claimant on a different quantity —
    /// the person the vehicle is registered to, on a published estimate rather
    /// than on a meter reading, and since the 2. THG-Novelle at `[38k §7(6)]`'s
    /// higher factor for an M3 or N3 vehicle. None of that is a fact a charge
    /// point holds, which is why it is somebody else's filing and not a branch
    /// here.
    #[error(
        "{evse_id} is not publicly accessible: `[38k §6]` counts only public points, and a \
         non-public point is claimed under `[38k §7]` by the vehicle keeper on an estimate"
    )]
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

    /// A record carries energy no meter signed, **or signed and did not verify**.
    ///
    /// `[38k §6(3) Nr. 2]` requires the energetic quantity to be determined in
    /// conformity with the measuring and calibration law. A record with no
    /// evidence behind it cannot show that, and a claim that included it would
    /// be one the third party's own declaration `[38k §6(4)]` contradicts.
    ///
    /// The two cases are one refusal because they fail the same paragraph — and
    /// the second is the worse of them. A record whose chain did not hold up
    /// *has* an `EvidenceRef`, so a check that asks only whether one exists
    /// passes it, and the kilowatt-hours of a session nothing verified reach a
    /// file whose whole claim is that only signed ones do.
    #[error(
        "{cdr} carries no energy a meter signed and this side verified: `[38k §6(3) Nr. 2]` \
         cannot be shown for it"
    )]
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

    /// A vehicle `[38k §7]` will not count.
    #[error("{reference} cannot be counted under `[38k §7]`: {missing}")]
    RegistrationNotEvidenced {
        /// The filer's own reference for the vehicle.
        reference: String,
        /// Which condition is not met.
        missing: String,
    },

    /// The estimate was published for a different vehicle class.
    ///
    /// `[38k §7(3)]` publishes a *Schätzwert* per class, and counting a bus at
    /// a car's estimate is inventing a quantity nobody announced.
    #[error(
        "{reference} is a {expected} and the estimate offered was announced for a {announced_for} `[38k §7(3)]`"
    )]
    NoEstimateForClass {
        /// The filer's own reference for the vehicle.
        reference: String,
        /// The class the vehicle is.
        expected: &'static str,
        /// The class the estimate was published for.
        announced_for: &'static str,
    },

    /// The same vehicle was counted twice in one obligation year.
    ///
    /// `[38k §7(4) S. 2]`: *"Die Anrechnung der dem jeweiligen Schätzwert
    /// entsprechenden Strommenge kann pro reinem Batterieelektrofahrzeug und
    /// pro Verpflichtungsjahr nur einmal erfolgen."*
    #[error(
        "{reference} is already in this notification: `[38k §7(4) S. 2]` counts a vehicle once per obligation year"
    )]
    VehicleAlreadyCounted {
        /// The filer's own reference for the vehicle.
        reference: String,
    },

    /// The same charge point was added to one notification twice.
    ///
    /// `[38k §8(1) S. 3]`: *"Mitteilungen nach Satz 1 Nummer 1 können für den
    /// jeweiligen Ladepunkt für das jeweilige Verpflichtungsjahr nur einmal
    /// erfolgen."* One notification per point per obligation year — so two
    /// lines for one point inside one notification is the same fault arriving a
    /// level down, and the second line silently replacing the first is how a
    /// year's energy goes missing without anything failing.
    #[error(
        "{evse_id} is already in this notification: `[38k §8(1) S. 3]` allows one notification per point per obligation year"
    )]
    PointAlreadyReported {
        /// The point, canonically spelled.
        evse_id: String,
    },
}
