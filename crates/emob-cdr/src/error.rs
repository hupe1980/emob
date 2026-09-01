//! What can go wrong building a CDR.

use emob_core::{Direction, Energy, IdentificationStrength};
use emob_session::{AuthPath, SessionError};

/// A CDR that could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CdrError {
    /// The session is still running.
    #[error("a CDR cannot be built from a session that has not ended")]
    SessionNotEnded,

    /// No key was given.
    #[error("a CDR needs a key: a party and an id unique within it")]
    NoKey,

    /// The meter series covers time the session does not.
    ///
    /// A period runs over the part of its settlement slot the readings actually
    /// covered, so readings outside the session window produce periods outside
    /// it — which `validate` blocks on an inbound record, and which the builder
    /// therefore refuses to emit. OCPP delivers `MeterValues` asynchronously,
    /// so a `StopTransaction` timestamp preceding the last reading is ordinary
    /// rather than exotic; the difference is time nobody can say whether the
    /// driver was there for, and clamping the window would invent it.
    #[error(
        "the meter series runs {} to {} but the session ran {} to {}: a CDR cannot claim measurement outside its own window, and clamping it would invent one",
        .metered.0, .metered.1, .session.0, .session.1
    )]
    ReadingsOutsideSession {
        /// The session's own window.
        session: (time::OffsetDateTime, time::OffsetDateTime),
        /// The window the readings cover.
        metered: (time::OffsetDateTime, time::OffsetDateTime),
    },

    /// The periods do not sum to the total.
    #[error("the periods sum to {periods} but the total is {total}")]
    DoesNotConserve {
        /// What the periods add up to.
        periods: Energy,
        /// What was claimed.
        total: Energy,
    },

    /// The signed record claims a stronger identification than the
    /// authorisation path can support.
    #[error(
        "the session claims {claimed} authorisation, which supports at most {ceiling} identification, but the signed record reports {signed}"
    )]
    AuthStrengthMismatch {
        /// The path the session claims.
        claimed: AuthPath,
        /// The strongest level that path can honestly support.
        ceiling: IdentificationStrength,
        /// What the signed record states.
        signed: IdentificationStrength,
    },

    /// The meter says energy moved in a slot the session says it was suspended
    /// for.
    #[error(
        "{energy} moved in the quarter hour beginning {at}, which the session records as suspended: the meter and the state machine disagree"
    )]
    EnergyWhileSuspended {
        /// The settlement slot.
        at: time::OffsetDateTime,
        /// What the meter recorded.
        energy: Energy,
    },

    /// The record claims one direction and the signed register measured the
    /// other.
    #[error(
        "the record claims {claimed} but the signed register measured {signed} [OCMF Tab. 25]: import and export never net, and one of the two is a V2G discharge"
    )]
    DirectionMismatch {
        /// What the record claims.
        claimed: Direction,
        /// What the signed OBIS register says.
        signed: Direction,
    },

    /// The tariff charges for time and the signed records do not support
    /// billing a duration.
    #[error(
        "the tariff charges for {dimension:?} but the signed records do not support billing a duration: the clock was not synchronised, or a fault flagged the time. The energy is unaffected — price this session per kWh"
    )]
    DurationNotBillable {
        /// Which time dimension the tariff prices.
        dimension: emob_tariff::Dimension,
    },

    /// The signed records do not support billing the energy at all.
    #[error(
        "the signed records this record rests on do not support billing its energy [MessEG §33]: a value that does not verify does not bill, and evidence that is present and failed is worse than evidence that is absent. Read `Evidence::reasons()` for what went wrong"
    )]
    EnergyNotBillable,

    /// The session is shorter than the station's clock can resolve.
    #[error(
        "the tariff charges for {dimension:?} but the session lasted {} s, below the {} s its clock can resolve [REA 6-A §3.1]: a measured value shorter than the shortest measurable span is not one an invoice may use. The energy is unaffected — price this session per kWh",
        measured.whole_seconds(),
        shortest.whole_seconds()
    )]
    DurationBelowClockResolution {
        /// Which time dimension the tariff prices.
        dimension: emob_tariff::Dimension,
        /// How long the session lasted.
        measured: time::Duration,
        /// The shortest span the clock may be billed for.
        shortest: time::Duration,
    },

    /// The tariff was not in force when the session started.
    #[error(
        "tariff {tariff_id} was not in force at {at} (valid {valid_from:?} to {valid_until:?}) [AFIR Art. 5(4)]: the price has to be known to the driver before the session starts, so the version in force then is the one that governs"
    )]
    TariffNotInForce {
        /// Which tariff.
        tariff_id: String,
        /// When the session started.
        at: time::OffsetDateTime,
        /// The first instant the tariff was in force, if bounded.
        valid_from: Option<time::OffsetDateTime>,
        /// The instant it stopped, if bounded.
        valid_until: Option<time::OffsetDateTime>,
    },

    /// No version of a tariff was in force when the session started.
    #[error(
        "no version of tariff {tariff_id} was in force at {at}: a gap in a price history is an interval nothing can be priced in, and guessing a price is not a fix"
    )]
    NoTariffInForce {
        /// Which tariff.
        tariff_id: String,
        /// When the session started.
        at: time::OffsetDateTime,
    },

    /// The session could not be split.
    #[error(transparent)]
    Session(#[from] SessionError),

    /// The record's periods do not form a session a tariff can price.
    #[error("the record cannot be priced: {0}")]
    NotChargeable(#[from] emob_tariff::ChargeableError),
}
