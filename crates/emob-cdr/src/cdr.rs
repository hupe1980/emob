//! The charge detail record: what two companies settle against.
//!
//! # Why a CDR is not just a session with a total on it
//!
//! A session is what happened. A CDR is a **claim** about what happened, sent
//! to somebody who was not there and who will pay against it. Four things
//! follow, and they are what this module enforces:
//!
//! 1. **It carries its own arithmetic.** The charging periods sum to the total,
//!    exactly, checked at construction — because the recipient will check, and
//!    finding out then costs a dispute.
//! 2. **It names its evidence.** Every CDR built here references the signed
//!    records it rests on by content digest, so "which meter values is this
//!    €14.46 made of" is answerable years later `[MessEG §33]`.
//! 3. **It carries its money, and the money comes from the same periods.**
//!    [`CdrBuilder::rated_with`] prices the record's own charging periods, so
//!    every euro traces to a quarter hour that traces to a signed reading.
//!    Rating in a separate service is how a CDR and its invoice line come to
//!    disagree about the same session.
//! 4. **It is immutable and identified.** A CDR that can be edited in place is
//!    a CDR whose recipient and sender can hold different versions of the same
//!    id — which is the most common way roaming settlement goes wrong.

use emob_core::{
    Activity, CdrId, ClockResolution, Direction, Energy, EvseId, IdentificationStrength, PartyId,
    SessionId, TariffId,
};
use emob_eichrecht::Evidence;
use emob_session::{AuthPath, Provenance, QuarterHour, Session, SessionSplit};
use emob_tariff::{Chargeable, Dimension, Period, Rated, Tariff, TariffFingerprint, TariffHistory};

use crate::error::CdrError;

/// One period of a CDR: a slice of the session with an energy attached.
///
/// Aligned to quarter hours when the CDR was built from a [`SessionSplit`],
/// because that is what the German pass-through model settles in `[A6 §IV.1]`.
///
/// The slot and the window are separate fields on purpose. A session that
/// starts at 10:07 has its first period reported under the quarter hour
/// beginning **10:00** — that is the settlement period the energy belongs to —
/// while the period itself runs from **10:07**, because that is when the
/// session began. Collapsing the two into one timestamp produces a record whose
/// first period starts before the session does, which every partner's validator
/// flags and no partner's validator can fix.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ChargingPeriod {
    /// The settlement slot this period is reported under.
    pub quarter_hour: QuarterHour,
    /// When the period begins — never before the session did.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub start: time::OffsetDateTime,
    /// When it ends — never after the session did.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub end: time::OffsetDateTime,
    /// The energy moved inside it.
    pub energy: Energy,
    /// What the session was doing during this period.
    ///
    /// A **stated fact, not a derived one**, and the distinction decides money:
    /// `[AFIR Art. 5(4)]` lets a fast charger add an occupancy fee per minute
    /// for the time a vehicle is connected and *not* charging, so a period on
    /// the wrong side of this is billed at the wrong rate.
    ///
    /// Deriving it from `energy == 0` gets a taper wrong in exactly the case
    /// that matters. A car at 100 % state of
    /// charge draws a rounding error, and a quarter hour that genuinely
    /// measured `0.000 kWh` while the session's own state machine says
    /// `Charging` is a taper, not an occupancy. [`CdrBuilder`] takes this from
    /// the session history instead.
    ///
    /// **Three values and not two**, because "no energy flowed" and "the driver
    /// was loitering" are different facts and only one of them owes a fee. See
    /// [`Activity`].
    pub activity: Activity,
    /// How the number was arrived at — measured, or interpolated between two
    /// readings. Travels to the recipient, because a settlement dispute turns
    /// on it.
    pub provenance: Provenance,
}

impl ChargingPeriod {
    /// How long the period lasted.
    #[must_use]
    pub fn duration(&self) -> time::Duration {
        self.end - self.start
    }
}

/// The globally unique key of a CDR.
///
/// OCPI makes a CDR id unique per `country_code`/`party_id`, not globally, so
/// the key is the triple and never the bare id. Two CPOs may each have a CDR
/// `1`, and a ledger keyed on the id alone will drop one of them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CdrKey {
    /// The CPO that owns the record.
    pub party: PartyId,
    /// The record's id within that party.
    pub id: CdrId,
}

impl core::fmt::Display for CdrKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}/{}", self.party, self.id)
    }
}

/// A reference to the signed evidence a CDR rests on.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EvidenceRef {
    /// The encoding the station used — `OCMF`, `Alfen Eichrecht`, …
    ///
    /// OCPI's `SignedData.encoding_method` field, kept because a recipient
    /// needs it to pick a verifier.
    pub encoding_method: String,
    /// SHA-256 of each signed payload, in order.
    ///
    /// Digests rather than the payloads themselves: a CDR travels through
    /// roaming and a full OCMF blob per reading makes it enormous. The payloads
    /// live in the evidence store, and these say which ones.
    pub payload_digests: Vec<[u8; 32]>,
    /// How strongly the driver was identified, as the signed record states it.
    ///
    /// The **weakest** level any record in the chain asserted.
    pub identification_strength: IdentificationStrength,
    /// Whether the signed records support billing the **energy** at all.
    ///
    /// The workspace's central promise, carried across the seam where it is
    /// otherwise lost: `Evidence::billable_energy()` returns `None` whenever a
    /// signature failed or the chain did not hold up, and a CDR's energy comes
    /// from the *session's* meter series rather than from the evidence — so
    /// without this field a record could be priced off a register nothing
    /// verified, while still carrying an `EvidenceRef` that made it look
    /// checked.
    ///
    /// Evidence that is present and failed is worse than evidence that is
    /// absent: the records exist and they do not hold up. [`CdrBuilder::build`]
    /// refuses it.
    pub energy_billable: bool,
    /// Whether the signed records support billing a *duration* as well as an
    /// energy — the clock was synchronised or relative `[OCMF Tab. 19]` and no
    /// `EF` flag marked the time unusable.
    ///
    /// A separate fact from the energy, because a session on an unsynchronised
    /// clock has a register an invoice may use and a duration it may not.
    pub duration_billable: bool,
    /// Which way the signed register says the energy flowed `[OCMF Tab. 25]`.
    ///
    /// `None` for a register whose OBIS code the verifier could not classify —
    /// which is not the same as import.
    pub direction: Option<Direction>,
    /// How much of the billed register is cable rather than vehicle, when the
    /// meter reported it `[OCMF Tab. 7, CL]`.
    ///
    /// Never subtracted from anything — the compensation is already inside the
    /// register value — and carried because a partner disputing the energy will
    /// ask how much of it was cable, and because `[REA 6-A §3.2]` makes telling
    /// the customer what is inside a measured value a duty rather than a
    /// courtesy. The chain has computed this since the register was read; until
    /// it reached the record it stopped at the crate that computed it, which is
    /// a fact modelled and never consulted.
    pub compensated_loss: Option<Energy>,
    /// The instants the signed records mark a **tariff change** at
    /// `[OCMF Tab. 7, TX=T]`, in the order the meter signed them.
    ///
    /// The station's own account of where its price changed. A record is priced
    /// by the version in force when the session started `[AFIR Art. 5(4)]`, so
    /// a change the meter signed *inside* the session is the station saying two
    /// prices applied to something this record prices with one —
    /// [`CdrBuilder::build`] refuses that rather than billing one of the two.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::rfc3339_list"))]
    pub tariff_changes: Vec<time::OffsetDateTime>,
}

impl EvidenceRef {
    /// Read the reference off a verified [`Evidence`] record.
    ///
    /// The only constructor a caller should normally reach for. Filling
    /// [`Self::identification_strength`] by hand makes the CDR builder's
    /// cross-check a formality — a hand-filled field can be filled with
    /// whatever value makes the record build, which is the opposite of a check.
    ///
    /// A session whose chain did not hold up reports no identification at all,
    /// which is [`IdentificationStrength::None`] — the weakest claim, and
    /// therefore one no authorisation path can be caught over-stating.
    #[must_use]
    pub fn from_evidence(evidence: &Evidence, encoding_method: impl Into<String>) -> Self {
        Self {
            encoding_method: encoding_method.into(),
            payload_digests: evidence.payload_digests(),
            identification_strength: evidence.identification_strength().unwrap_or_default(),
            energy_billable: evidence.is_billable(),
            duration_billable: evidence.is_billable_for_time(),
            direction: evidence.direction(),
            compensated_loss: evidence.compensated_loss(),
            tariff_changes: evidence.tariff_change_instants(),
        }
    }
}

/// What a CDR costs, and which tariff said so.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cost {
    /// The tariff that priced it, by name.
    pub tariff_id: TariffId,
    /// …and by **content**.
    ///
    /// A tariff id is a name, and names get reused: a CPO that edits a tariff
    /// in place keeps the id, so a record naming only the id names something
    /// that no longer exists. A partner re-rating it six weeks later gets a
    /// different total and cannot tell an honest price change from a restated
    /// one.
    ///
    /// The fingerprint is the same answer the evidence chain gives one layer
    /// down, where a CDR names its meter records by digest rather than by
    /// reference. [`Cdr::was_priced_with`] is the check it enables.
    pub tariff_fingerprint: TariffFingerprint,
    /// Every line of the price, its VAT breakdown and any note the rating had
    /// to make.
    pub rated: Rated,
    /// The **reservation** that preceded the session, priced separately.
    ///
    /// A second [`Rated`] rather than lines inside the first, because
    /// `[OCPI 2.3.0]` keeps the two apart at every level — a restriction rather
    /// than a dimension, a window that ran before any energy moved, and its own
    /// `total_reservation_cost`. A tariff whose unrestricted element prices
    /// `TIME` and whose reservation element also prices `TIME` would otherwise
    /// have the two competing for one dimension.
    ///
    /// `None` for the ordinary session nobody reserved. See
    /// [`emob_tariff::rate_reservation`].
    pub reservation: Option<Rated>,
}

impl Cost {
    /// What the session and its reservation come to together.
    ///
    /// The figure a driver pays and `total_cost` states. The two ratings round
    /// separately, because each is a document the other does not appear in —
    /// which is the same reason `[OCPI 2.3.0]` gives them two fields.
    ///
    /// # The currency is the session's, and neither term is dropped
    ///
    /// Both ratings come from one [`emob_tariff::Tariff`] wherever this
    /// workspace builds a record, so they are one currency by construction; a
    /// record where they are not is one [`crate::validate()`] blocks
    /// (`Finding::CostCurrencyMismatch`), and `emob-billing` refuses it with
    /// every other unsettleable shape.
    ///
    /// So the two are **added** rather than one of them being given up on: a
    /// total short by a term the record states is a plausible number in the
    /// right currency, and it reaches the invoice and the ledger's restatement
    /// comparison alike (D250).
    #[must_use]
    pub fn gross(&self) -> emob_core::Money {
        let session = self.rated.gross();
        self.reservation.as_ref().map_or(session, |reservation| {
            emob_core::Money::new(
                session.amount() + reservation.gross().amount(),
                session.currency(),
            )
        })
    }

    /// Whether the record's two ratings are quoted in one currency.
    ///
    /// True by construction for a record this workspace priced, and the
    /// precondition [`Self::gross`] rests on. [`crate::validate()`] asks it of a
    /// record somebody else built.
    #[must_use]
    pub fn currencies_agree(&self) -> bool {
        self.reservation
            .as_ref()
            .is_none_or(|reservation| reservation.currency == self.rated.currency)
    }
}

/// A charge detail record.
///
/// Immutable once built. A correction is a new CDR that supersedes this one,
/// which is what OCPI's own model assumes and what makes an audit trail
/// possible.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cdr {
    /// Its unique key.
    pub key: CdrKey,
    /// The **reservation** this session was started under, when there was one.
    ///
    /// A fact about the record rather than about the session: the window ran
    /// before the cable went in, so no meter measured it. Priced separately —
    /// see [`Cost::reservation`] — and carried in the field
    /// `[OCPI 2.3.0 §mod_cdrs_cdr_object]` keeps for it.
    ///
    /// A reservation that **expired** never became a session, so it never
    /// becomes one of these: OCPI lets such a record omit its `session_id`, and
    /// a CDR with no session is a document a service assembles rather than one
    /// this builder derives from a `Session`.
    pub reservation: Option<emob_tariff::Reservation>,
    /// The session it records.
    pub session_id: SessionId,
    /// Where it happened.
    pub evse_id: EvseId,
    /// When the session started.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub started_at: time::OffsetDateTime,
    /// When it ended.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub ended_at: time::OffsetDateTime,
    /// How it was authorised.
    pub auth_path: AuthPath,
    /// The reference the **provider** gave when it authorised the session, when
    /// there was one.
    ///
    /// OCPI carries it on the Session and on the CDR
    /// `[OCPI 2.3.0 §mod_cdrs_cdr_object]` — *"Reference to the authorization
    /// given by the eMSP"* — and it is the eMSP's own handle on the decision it
    /// made. Without it on the record, a provider settling a CDR cannot tie it
    /// to the `Authorize` it answered: it has the session id the **CPO**
    /// invented and nothing of its own. The session has carried this since the
    /// authorisation paths were modelled and no seam read it, which is a field
    /// stored and never consulted.
    pub authorization_reference: Option<String>,
    /// The periods, in time order, summing to [`Self::total_energy`].
    pub periods: Vec<ChargingPeriod>,
    /// The total. Equal to the sum of the periods, checked at construction.
    pub total_energy: Energy,
    /// Which way the energy flowed.
    pub direction: Direction,
    /// The shortest span the station's clock may be billed for
    /// `[REA 6-A §3.1]`, from its type approval.
    ///
    /// # Why it is on the record rather than only in the builder
    ///
    /// It decides a price. A duration below it is not a measured value an
    /// invoice may use, so the rating drops that line — and a record priced by
    /// a station whose approval states ten seconds bills half a minute of
    /// occupancy that the regulation's sixty-second cap would refuse.
    ///
    /// A dispute two years later is answered by replaying the pricing exactly
    /// as it ran, and a replay that has to be *told* the resolution is a replay
    /// that can be told the wrong one. So the record carries it, and
    /// [`Self::rerated_with`] reads it rather than taking it again.
    ///
    /// [`ClockResolution::conforming`] — the regulation's own cap — for a
    /// record read back from a partner, because no roaming wire carries the
    /// field and the worst case the regulation permits is the honest answer for
    /// an approval this side has not read.
    #[cfg_attr(feature = "serde", serde(default))]
    pub clock: ClockResolution,
    /// The signed records this rests on, when there are any.
    pub evidence: Option<EvidenceRef>,
    /// What it costs, when it has been rated.
    pub cost: Option<Cost>,
    /// The CDR this one supersedes, for a correction.
    pub supersedes: Option<CdrKey>,
}

impl Cdr {
    /// Whether the periods sum to the total, exactly.
    ///
    /// True by construction — [`CdrBuilder::build`] refuses otherwise — and
    /// re-checkable, because a CDR that arrived over the wire was built by
    /// somebody else's code.
    #[must_use]
    pub fn conserves(&self) -> bool {
        self.periods.iter().map(|p| p.energy).sum::<Energy>() == self.total_energy
    }

    /// How long the session lasted.
    #[must_use]
    pub fn duration(&self) -> time::Duration {
        self.ended_at - self.started_at
    }

    /// Whether every period's energy is exact — measured, or held at a
    /// measurement across an interval the session says nothing flowed in —
    /// rather than interpolated.
    #[must_use]
    pub fn fully_measured(&self) -> bool {
        self.periods.iter().all(|p| p.provenance.is_exact())
    }

    /// Whether this CDR is backed by signed evidence.
    ///
    /// A CDR without it may be perfectly good telemetry and may not be the
    /// basis of an energy invoice in Germany `[MessEG §33]`.
    #[must_use]
    pub const fn has_evidence(&self) -> bool {
        self.evidence.is_some()
    }

    /// What the driver or partner pays, when the record has been rated.
    #[must_use]
    pub fn total_cost(&self) -> Option<emob_core::Money> {
        self.cost.as_ref().map(Cost::gross)
    }

    /// Whether this record was priced with **this exact** tariff.
    ///
    /// The question a receiving party actually has before it re-rates: not
    /// "does the id match" — ids are reused — but "is the tariff I hold the one
    /// that produced these euros". A `false` here says the two sides are
    /// looking at different documents, which is worth knowing *before* the
    /// totals are compared and found to differ.
    #[must_use]
    pub fn was_priced_with(&self, tariff: &Tariff) -> bool {
        self.cost
            .as_ref()
            .is_some_and(|c| c.tariff_fingerprint == tariff.fingerprint())
    }

    /// Price this record again, with another party's tariff.
    ///
    /// # The other half of roaming
    ///
    /// A CPO issues a CDR priced with its own tariff. The eMSP that receives it
    /// owes its **driver** a different number — its own retail price — and owes
    /// the CPO a comparison. `emob_roam::ocpi::from_ocpi` therefore lands a
    /// partner's record **unpriced**, and this is what prices it.
    ///
    /// It exists rather than being left to the caller because the composition
    /// is where the gates get skipped. Reaching for [`Self::chargeable`] and
    /// [`emob_tariff::rate`] directly — the obvious way to do it — silently
    /// drops all four: a retail tariff that was not in force when the session
    /// ran, a version the meter says was superseded mid-session, a duration the
    /// signed records do not vouch for, and the clock resolution
    /// `[REA 6-A §3.1]` puts under a per-minute fee. An eMSP re-rating a
    /// hundred thousand partner records a month with none of them is the same
    /// class of failure as a CPO issuing them without: every unit test passes
    /// and the composition does not (rule 5).
    ///
    /// The clock resolution is the **record's own** — [`Self::clock`] — rather
    /// than an argument. It is what the station's type approval states, no
    /// roaming wire carries the field, and `emob_roam::ocpi::from_ocpi` sets
    /// [`ClockResolution::conforming`] on a partner's record: the worst case the
    /// regulation permits, which is the honest answer for an approval this side
    /// has not read. A caller that *has* read one states it on the record it
    /// re-rates, where a reader can see it, rather than in a call nobody keeps.
    ///
    /// The periods, the energy, the evidence and the key are the record's own —
    /// only the price changes, so the two numbers are about the same session by
    /// construction and the comparison is a comparison.
    ///
    /// # Errors
    ///
    /// Every gate [`CdrBuilder::build`] applies to a price:
    /// [`CdrError::TariffNotInForce`] for a version that did not govern the
    /// session, [`CdrError::SignedTariffChangeInsideSession`] where the meter
    /// says another applied inside it, [`CdrError::DurationNotBillable`] where
    /// the price charges for time the signed records do not vouch for, and
    /// [`CdrError::NotChargeable`] where the periods do not form a session.
    pub fn rerated_with(&self, tariff: &Tariff) -> Result<Self, CdrError> {
        Ok(Self {
            cost: Some(priced(self, tariff)?),
            ..self.clone()
        })
    }

    /// The session, in the terms a tariff prices it.
    ///
    /// The bridge between the settlement grid and the price: a CDR's periods
    /// *are* the rating periods, so a re-rating by the receiving party reads
    /// exactly the same slices the issuer did — through
    /// [`Self::rerated_with`], which is the same door the issuer used.
    ///
    /// # Errors
    ///
    /// [`CdrError::NotChargeable`] when the periods cannot form a session — an
    /// empty record, or one whose periods overlap.
    ///
    /// Carries [`Self::clock`], so a caller cannot rate a record at a resolution
    /// the record does not state.
    pub fn chargeable(&self) -> Result<Chargeable, CdrError> {
        let periods = self
            .periods
            .iter()
            .map(|p| Period {
                start: p.start,
                end: p.end,
                energy: p.energy,
                // Read, not re-derived. The record states what the session
                // was doing; inferring it from a zero energy would bill a
                // taper at the occupancy rate.
                activity: p.activity,
            })
            .collect();
        Chargeable::new(periods)
            .map(|chargeable| chargeable.with_clock(self.clock))
            .map_err(CdrError::NotChargeable)
    }
}

/// Builds a CDR from a session, refusing to produce one that does not add up.
///
/// ```no_run
/// use emob_cdr::CdrBuilder;
/// # let session: emob_session::Session = unimplemented!();
/// # let party: emob_core::PartyId = unimplemented!();
/// # let evidence_ref: emob_cdr::EvidenceRef = unimplemented!();
/// # let tariff: emob_tariff::Tariff = unimplemented!();
///
/// let cdr = CdrBuilder::from_session(&session, emob_core::Direction::Import)?
///     .key(party, "cdr-1".parse()?)
///     .evidence(evidence_ref)
///     .rated_with(&tariff)
///     .build()?;
///
/// assert!(cdr.conserves());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct CdrBuilder<'a> {
    key: Option<CdrKey>,
    session_id: SessionId,
    evse_id: EvseId,
    started_at: time::OffsetDateTime,
    ended_at: time::OffsetDateTime,
    auth_path: AuthPath,
    authorization_reference: Option<String>,
    split: SessionSplit,
    /// What the session was doing in each slot, in the split's order — read off
    /// the session's state machine rather than guessed from the energy.
    activities: Vec<Activity>,
    /// The parts of the session window the meter series does not cover, already
    /// cut at the settlement grid and at the session's own state changes, with
    /// each one's activity read off the same state machine.
    unmetered: Vec<ChargingPeriod>,
    clock: ClockResolution,
    evidence: Option<EvidenceRef>,
    tariff: Option<&'a Tariff>,
    reservation: Option<emob_tariff::Reservation>,
    supersedes: Option<CdrKey>,
}

impl<'a> CdrBuilder<'a> {
    /// Start from a finished session.
    ///
    /// # Errors
    ///
    /// [`CdrError::SessionNotEnded`] when the session is still running — a CDR
    /// for a session in progress is a claim about a number that is still
    /// changing.
    /// [`CdrError::Session`] when the session cannot be split,
    /// [`CdrError::ReadingsOutsideSession`] when the meter series covers time
    /// the session does not, or [`CdrError::EnergyWhileNotCharging`] when a
    /// slot moved energy the session says it was not charging in.
    pub fn from_session(session: &Session, direction: Direction) -> Result<Self, CdrError> {
        let ended_at = session.ended_at.ok_or(CdrError::SessionNotEnded)?;
        let split = session.split(direction)?;

        // A period runs over the part of its slot the meter series covered, so
        // a series that reaches outside the session produces a record whose
        // periods do — and `validate` blocks exactly that on the way in. The
        // builder's promise is that it refuses what it would refuse from
        // somebody else, so it has to ask the same question of itself.
        //
        // It is a real fault rather than a formality: OCPP delivers
        // `MeterValues` asynchronously, so a `StopTransaction` timestamp can
        // easily precede the last reading, and the difference is time nobody
        // can say whether the driver was there for. Clamping the window would
        // invent one, which is the mutation this crate refuses everywhere else.
        let (metered_from, metered_to) = (split.slots.first(), split.slots.last());
        if let (Some(first), Some(last)) = (metered_from, metered_to)
            && (first.from < session.started_at || last.to > ended_at)
        {
            return Err(CdrError::ReadingsOutsideSession {
                session: (session.started_at, ended_at),
                metered: (first.from, last.to),
            });
        }

        // The session's own history says when it was charging; the meter says
        // how much moved. The split is cut at every transition, so each slot
        // lies inside one state, and `charging_throughout` is the one question
        // asked of every period this record carries — metered here, unmetered
        // below — because a period is charging when the operator's own record
        // says it was, and occupancy otherwise. `Pending` is a car connected
        // and authorised with nothing flowing, which is exactly what
        // `[AFIR Art. 5(4)]`'s fee prices; reading it as charging because the
        // session had not yet said "suspended" billed that minute as charging
        // time.
        //
        // When the two disagree — energy across a slot the session says moved
        // none — one of them is wrong, and guessing which is how a driver is
        // billed for a charge the operator's own records say never happened.
        // The split already holds the register flat across the session's idle
        // intervals, so this can only fire where the meter itself moved with
        // no charging time to attribute it to: a real contradiction rather
        // than one a straight line invented.
        let mut activities = Vec::with_capacity(split.slots.len());
        for slot in &split.slots {
            // A slot the history does not answer with one word — `Pending`,
            // `Ended`, or one straddling a transition the split failed to cut
            // at — is a car connected and authorised with nothing flowing,
            // which is what `[AFIR Art. 5(4)]`'s fee prices.
            let activity = session
                .activity_throughout(slot.from, slot.to)
                .unwrap_or(Activity::Parked);
            if !activity.transfers_energy() && !slot.energy.is_zero() {
                return Err(CdrError::EnergyWhileNotCharging {
                    at: slot.from,
                    state: session.state_at(slot.from),
                    energy: slot.energy,
                });
            }
            activities.push(activity);
        }

        // The meter series spans the readings; the session spans the parking
        // space. A car that finishes charging at 11:00 and is collected at
        // 13:00 leaves two hours the split knows nothing about, and those two
        // hours are exactly what `[AFIR Art. 5(4)]`'s occupancy fee is for.
        //
        // The gap is filled **here**, where the session is still in scope,
        // because its `charging` flag is the same stated fact as every other
        // period's and has to come from the same place. Filled in `build()` it
        // could only be derived — from the absence of readings — and the flag
        // this record carries is the one the article prices, so a gap the
        // operator's own state machine calls charging would have been billed as
        // occupancy on the strength of a meter that said nothing at all.
        let metered_from = split.slots.first().map_or(ended_at, |slot| slot.from);
        let metered_to = split
            .slots
            .last()
            .map_or(session.started_at, |slot| slot.to);
        let mut unmetered = unmetered_periods(session, session.started_at, metered_from);
        unmetered.extend(unmetered_periods(session, metered_to, ended_at));

        Ok(Self {
            key: None,
            session_id: session.id.clone(),
            evse_id: session.evse_id.clone(),
            started_at: session.started_at,
            ended_at,
            auth_path: session.authorization.path,
            authorization_reference: session.authorization.authorization_reference.clone(),
            split,
            activities,
            unmetered,
            clock: ClockResolution::default(),
            evidence: None,
            tariff: None,
            reservation: None,
            supersedes: None,
        })
    }

    /// The reservation this session was started under.
    ///
    /// Its window ran before the cable went in, so it is not a period of the
    /// session and no meter measured it. Rated separately, against the elements
    /// `[OCPI 2.3.0]` restricts to a reservation, and carried in the field the
    /// specification keeps for it. See [`Cost::reservation`].
    #[must_use]
    pub fn reserved(mut self, reservation: emob_tariff::Reservation) -> Self {
        self.reservation = Some(reservation);
        self
    }

    /// Give the CDR its key.
    #[must_use]
    pub fn key(mut self, party: PartyId, id: CdrId) -> Self {
        self.key = Some(CdrKey { party, id });
        self
    }

    /// State what the station's clock can actually resolve.
    ///
    /// `[REA 6-A §3.1]`: "Messwerte unterhalb der kürzest möglichen Zeitspanne
    /// werden nicht für Abrechnungszwecke verwendet." The figure comes from the
    /// device's type approval, and until it does the builder assumes the worst
    /// case the regulation permits — sixty seconds — because it has not been
    /// told the device is better than that. A duration below it is not billed:
    /// the rating drops the time line and notes why, and the energy is
    /// untouched — see [`emob_tariff::RatingNote::DurationBelowResolution`].
    #[must_use]
    pub const fn clock(mut self, clock: ClockResolution) -> Self {
        self.clock = clock;
        self
    }

    /// Attach the signed evidence this CDR rests on.
    #[must_use]
    pub fn evidence(mut self, evidence: EvidenceRef) -> Self {
        self.evidence = Some(evidence);
        self
    }

    /// Price the record with a tariff.
    ///
    /// The rating reads the CDR's own charging periods, so the money and the
    /// kilowatt-hours cannot have come from different views of the session.
    ///
    /// [`CdrBuilder::build`] refuses a tariff that was not in force when the
    /// session started — see [`Self::rated_with_history`].
    #[must_use]
    pub fn rated_with(mut self, tariff: &'a Tariff) -> Self {
        self.tariff = Some(tariff);
        self
    }

    /// Price the record with whichever version of a tariff governed the
    /// session.
    ///
    /// `[AFIR Art. 5(4)]` requires the ad-hoc price to be "known to end users
    /// **before they initiate** a recharging session", which settles what a
    /// tariff change mid-session otherwise leaves open: the governing version
    /// is the one in force when the session **started**, because that is the
    /// one the driver was shown. Variation *within* a version — a night rate, a
    /// tier — is still the element restrictions' job.
    ///
    /// # Errors
    ///
    /// [`CdrError::NoTariffInForce`] when the history has no version covering
    /// the session's start, which is a gap somebody has to close rather than a
    /// price to guess.
    pub fn rated_with_history(mut self, history: &'a TariffHistory) -> Result<Self, CdrError> {
        let tariff =
            history
                .in_force_at(self.started_at)
                .ok_or_else(|| CdrError::NoTariffInForce {
                    tariff_id: history.id().to_string(),
                    at: self.started_at,
                })?;
        self.tariff = Some(tariff);
        Ok(self)
    }

    /// Mark this CDR as superseding another.
    #[must_use]
    pub fn supersedes(mut self, previous: CdrKey) -> Self {
        self.supersedes = Some(previous);
        self
    }

    /// Build it.
    ///
    /// # Errors
    ///
    /// [`CdrError::NoKey`] when no key was given,
    /// [`CdrError::AuthStrengthMismatch`] when the session claims a stronger
    /// authorisation than its own signed record supports, or
    /// [`CdrError::DoesNotConserve`] when the periods lost energy on the way in.
    pub fn build(self) -> Result<Cdr, CdrError> {
        let key = self.key.ok_or(CdrError::NoKey)?;

        // The cross-check nobody runs. A session that *claims* Plug & Charge
        // and whose signed record reports a bare RFID UID is telling two
        // stories about one event, and the weaker one is the one with a
        // signature behind it. Billing the stronger claim — a PnC tariff, a
        // contract that was never presented — is the kind of error that only
        // surfaces when a driver disputes it.
        if let Some(evidence) = &self.evidence {
            // The promise the whole workspace is built on, at the one seam
            // where it was not being kept: a CDR's energy comes from the
            // session's meter series, so without this the record would be
            // priced off a register nothing verified while carrying an
            // `EvidenceRef` that made it look checked.
            if !evidence.energy_billable {
                return Err(CdrError::EnergyNotBillable);
            }

            let ceiling = self.auth_path.strongest_plausible_level();
            if evidence.identification_strength > ceiling {
                return Err(CdrError::AuthStrengthMismatch {
                    claimed: self.auth_path,
                    ceiling,
                    signed: evidence.identification_strength,
                });
            }

            // The invariant this workspace claims everywhere and could not
            // check until the OBIS code was read rather than carried: import
            // and export never net. `[OCMF Tab. 25]` reserves `B*` for import
            // and `C*` for export, so the signed register states the direction
            // — and a record claiming a draw over a `C2` register is a V2G
            // discharge being billed as consumption.
            if let Some(signed) = evidence.direction
                && signed != self.split.direction
            {
                return Err(CdrError::DirectionMismatch {
                    claimed: self.split.direction,
                    signed,
                });
            }
        }

        // A period is reported under its settlement slot and runs over the part
        // of that slot the meter series actually covered. For a session whose
        // readings begin at 10:07 the first period is the slot beginning 10:00
        // and the window 10:07–10:15, which is the only pair of statements that
        // are both true.
        //
        // The window comes from the slot, not from the quarter hour clamped to
        // the session. The two differ whenever the session is wider than its
        // readings — a station that authorises at 10:00 and sends its first
        // meter value at 10:20 — and clamping would claim twenty minutes of
        // measurement that never happened, then leave the occupancy fill below
        // with nothing to do.
        let (started_at, ended_at) = (self.started_at, self.ended_at);
        let activities = &self.activities;
        let mut periods: Vec<ChargingPeriod> = self
            .split
            .slots
            .iter()
            .enumerate()
            .map(|(index, slot)| ChargingPeriod {
                quarter_hour: slot.quarter_hour,
                start: slot.from,
                end: slot.to,
                energy: slot.energy,
                activity: activities.get(index).copied().unwrap_or(Activity::Charging),
                provenance: slot.provenance,
            })
            .collect();

        // …and the parts of the window the meter said nothing about, worked out
        // in `from_session` where the session's own history was still in scope.
        // A record that stops at the last reading cannot bill the two hours a
        // car sat there after finishing, and one that stretches the last reading
        // over them bills energy that did not flow.
        periods.extend(self.unmetered);
        periods.sort_by_key(|p| p.start);

        let mut cdr = Cdr {
            key,
            reservation: self.reservation,
            session_id: self.session_id,
            evse_id: self.evse_id,
            started_at,
            ended_at,
            auth_path: self.auth_path,
            authorization_reference: self.authorization_reference,
            periods,
            total_energy: self.split.total,
            direction: self.split.direction,
            clock: self.clock,
            evidence: self.evidence,
            cost: None,
            supersedes: self.supersedes,
        };

        // The split conserves by construction, so this can only fail if the
        // mapping above lost a slot. Checking anyway is the difference between
        // finding that here and finding it in a partner's reconciliation.
        if !cdr.conserves() {
            return Err(CdrError::DoesNotConserve {
                periods: cdr.periods.iter().map(|p| p.energy).sum::<Energy>(),
                total: cdr.total_energy,
            });
        }

        if let Some(tariff) = self.tariff {
            cdr.cost = Some(priced(&cdr, tariff)?);
        }

        Ok(cdr)
    }
}

/// Price a record with a tariff, applying every gate that stands between a
/// session and a number somebody pays.
///
/// The **one** place a `Cost` is made, so that a CDR this workspace builds and
/// one an eMSP re-rates from a partner pass through the same rules. Pricing a
/// record by reaching for [`Cdr::chargeable`] and [`emob_tariff::rate`] directly
/// skips all four of them, which is why [`Cdr::rerated_with`] exists rather than
/// leaving the composition to a caller.
///
/// # Errors
///
/// [`CdrError::TariffNotInForce`] for a version that did not govern the session,
/// [`CdrError::SignedTariffChangeInsideSession`] where the meter says another
/// version applied inside it, [`CdrError::DurationNotBillable`] where the price
/// charges for time the signed records do not vouch for, and
/// [`CdrError::NotChargeable`] where the periods do not form a session.
fn priced(cdr: &Cdr, tariff: &Tariff) -> Result<Cost, CdrError> {
    // A tariff that was not in force when the session started did not price it,
    // whatever its numbers say. `[AFIR Art. 5(4)]`: the price has to be known
    // to the driver before they start, so the version in force at that instant
    // is the one that governs.
    // The first gate, and the one that was in the builder alone. `priced` is the
    // **one door** a `Cost` is made through (D206) — and the other caller of it
    // re-rates a record that never passed the builder, because
    // `emob_roam::ocpi::from_ocpi` assembles a partner's document into a `Cdr`
    // directly. So a record whose chain did not verify could be priced at this
    // side's retail tariff and invoiced to a driver: D70's settled record for a
    // forged session, reached through the door D206 opened (D231).
    //
    // Evidence that is **absent** is a different question and still passes here:
    // which regime a record with no signature falls under is the billing layer's,
    // and `validate` reports it. Evidence that is present and failed is worse
    // than evidence that is absent, and this is where that is enforced.
    if let Some(evidence) = &cdr.evidence
        && !evidence.energy_billable
    {
        return Err(CdrError::EnergyNotBillable);
    }

    if !tariff.covers(cdr.started_at) {
        return Err(CdrError::TariffNotInForce {
            tariff_id: tariff.id.to_string(),
            at: cdr.started_at,
            valid_from: tariff.valid_from,
            valid_until: tariff.valid_until,
        });
    }

    // …and the station's own account of where its price changed. A record is
    // priced by one version — the one in force when the session started — so a
    // `TX=T` the meter signed *inside* the session is the signature component
    // saying two prices applied to something this record prices with one.
    // Billing either of them is picking a number over a signed statement that
    // contradicts it.
    //
    // The marker is optional and most stations never emit one, so its absence
    // says nothing; its presence is the one case where the price this workspace
    // computed is contradicted by evidence.
    if let Some(evidence) = &cdr.evidence
        && let Some(&at) = evidence
            .tariff_changes
            .iter()
            .find(|&&at| at > cdr.started_at && at < cdr.ended_at)
    {
        return Err(CdrError::SignedTariffChangeInsideSession {
            at,
            on_settlement_boundary: QuarterHour::is_boundary(at),
        });
    }

    // The clock's resolution travels with the session: a span the clock cannot
    // resolve is a measured value an invoice may not use `[REA 6-A §3.1]`, and
    // the rating drops that line — and only that line — with a note saying so.
    // A thirty-second wait before a charge begins is the ordinary shape of a
    // transaction that opens `EVConnected`, and it must not make the
    // kilowatt-hours unbillable over five cents of occupancy.
    let chargeable = cdr.chargeable()?;
    let rated = emob_tariff::rate(tariff, &chargeable);

    // The duration gate below asks about the duration this record **charges
    // for**, not about the dimensions the tariff mentions.
    //
    // The difference is a false refusal that costs real revenue: a tariff whose
    // occupancy fee begins after four hours prices `ParkingTime` somewhere, and
    // a thirty-minute session under it charges no duration at all. Refusing to
    // build that record — because the *tariff* names a time dimension — throws
    // away the kilowatt-hours as well, over a fee nobody was charged.
    //
    // It is also the rule [`crate::validate()`] already applies to a record
    // somebody else built, and a builder stricter than its own validator is a
    // builder that refuses records it would accept.
    for dimension in charged_durations(&rated) {
        // `[OCMF Tab. 19]` states how far the station's clock can be trusted,
        // and `[OCMF Tab. 7, EF]` flags a time value as unusable separately
        // from an energy one. Billing a duration off a clock the signed record
        // does not vouch for is billing a number nobody can defend. The energy
        // is unaffected, so the fix is a per-kWh tariff rather than a blocked
        // session.
        if let Some(evidence) = &cdr.evidence
            && !evidence.duration_billable
        {
            return Err(CdrError::DurationNotBillable { dimension });
        }
    }

    // The reservation, against the elements restricted to one. It is not
    // gated on the evidence: no meter measured it, and the Eichrecht gates are
    // about measured values. What stands behind it is the operator's own
    // record of when the point was held, which is the same class of fact as
    // the `chargingState` an occupancy fee already rests on.
    let reservation = cdr
        .reservation
        .as_ref()
        .map(|held| emob_tariff::rate_reservation(tariff, held));

    Ok(Cost {
        tariff_id: tariff.id.clone(),
        tariff_fingerprint: tariff.fingerprint(),
        rated,
        reservation,
    })
}

/// The time dimensions a rating actually charged for, in the order
/// `[AFIR Art. 5(4)]` prescribes.
///
/// Read off the rated lines rather than off the tariff, because the question
/// the Eichrecht gate asks is "does this record bill a duration", and a tariff
/// that prices one under conditions this session never met does not.
fn charged_durations(rated: &Rated) -> Vec<Dimension> {
    [Dimension::Time, Dimension::ParkingTime]
        .into_iter()
        .filter(|&dimension| !rated.base_quantity_for(dimension).is_zero())
        .collect()
}

/// The part of a session window the meter series does not cover, as periods
/// that moved no energy.
///
/// Cut at the settlement boundaries like everything else, so an interval that
/// crosses 11:15 is two periods and the market side sees the same grid the
/// energy did — **and** at every state change the session recorded inside it,
/// so each piece has one answer to "was the vehicle charging" rather than a
/// quarter hour that held two.
///
/// # The flag is read, not assumed
///
/// It is tempting to call unmetered time occupancy by definition: no readings,
/// no energy, so the vehicle was sitting there — which is what
/// `[AFIR Art. 5(4)]`'s fee prices. But "the meter said nothing" and "the
/// vehicle was not charging" are different claims, and only the second is one
/// the operator's own records make. A station that stops sending `MeterValues`
/// while its session state machine still says `Charging` produces a gap that
/// would be billed per minute as occupancy on the strength of an absence — the
/// same shape as inferring the flag from `energy == 0`, which this crate refuses
/// one field away.
///
/// So the state machine answers — [`Session::charging_throughout`], the same
/// question the metered periods are asked, because a piece is charging when the
/// operator's own record says it was, and occupancy otherwise. `Pending` and
/// `Ended` are neither charging nor suspended, and both are exactly the
/// "connected and not charging" the fee is for. The energy is `ZERO` and
/// [`Provenance::Interpolated`] either way, because that zero *is* assumed from
/// the absence of readings.
fn unmetered_periods(
    session: &Session,
    from: time::OffsetDateTime,
    to: time::OffsetDateTime,
) -> Vec<ChargingPeriod> {
    let mut periods = Vec::new();
    if to <= from {
        return periods;
    }

    // The grid boundaries and the session's own transitions, together, in one
    // ascending list — the same construction `emob_session::split` uses, for
    // the same reason: a boundary that changes the answer has to be a cut.
    let mut cuts: Vec<time::OffsetDateTime> = Vec::new();
    let mut slot = QuarterHour::containing(from);
    while slot.start() < to {
        if slot.start() > from {
            cuts.push(slot.start());
        }
        slot = slot.next();
    }
    cuts.extend(
        session
            .history()
            .iter()
            .map(|change| change.at)
            .filter(|at| *at > from && *at < to),
    );
    cuts.sort_unstable();
    cuts.dedup();

    let mut start = from;
    for end in cuts.into_iter().chain(core::iter::once(to)) {
        if end > start {
            periods.push(ChargingPeriod {
                quarter_hour: QuarterHour::containing(start),
                start,
                end,
                energy: Energy::ZERO,
                activity: session
                    .activity_throughout(start, end)
                    .unwrap_or(Activity::Parked),
                provenance: Provenance::Interpolated,
            });
        }
        start = end;
    }
    periods
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_core::{Currency, IdentificationStrength};
    use emob_session::{
        Authorization, EndReason, MeterReading, MeterSeries, ReadingContext, Session,
    };
    use emob_tariff::{Dimension, PriceComponent, TariffKind};
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use time::macros::datetime;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn kwh(s: &str) -> Energy {
        Energy::from_kwh(dec(s)).unwrap()
    }

    fn at(minute: i64) -> time::OffsetDateTime {
        datetime!(2026-01-02 10:00 +1) + time::Duration::minutes(minute)
    }

    fn party() -> PartyId {
        PartyId::new("DE", "ABC").unwrap()
    }

    fn ended_session() -> Session {
        let mut s = Session::open(
            "s-1".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(0),
        );
        s.transition_to(emob_session::SessionState::Charging, at(0))
            .unwrap();
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        at(15),
                        kwh("110.000"),
                        Direction::Import,
                        ReadingContext::SampleClock,
                    ),
                    MeterReading::new(
                        at(30),
                        kwh("118.000"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        s.end(at(30), EndReason::Local).unwrap();
        s
    }

    /// The same session, with the car left in the bay for another half hour
    /// after the charge finished — the thirty minutes `[AFIR Art. 5(4)]`'s
    /// occupancy fee is for.
    fn occupied_session() -> Session {
        let mut s = Session::open(
            "s-1".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(0),
        );
        s.transition_to(emob_session::SessionState::Charging, at(0))
            .unwrap();
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        at(30),
                        kwh("118.000"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        s.transition_to(emob_session::SessionState::SuspendedByVehicle, at(30))
            .unwrap();
        s.end(at(60), EndReason::Local).unwrap();
        s
    }

    fn evidence(strength: IdentificationStrength) -> EvidenceRef {
        EvidenceRef {
            encoding_method: "OCMF".into(),
            payload_digests: vec![[1u8; 32], [2u8; 32]],
            identification_strength: strength,
            energy_billable: true,
            duration_billable: true,
            direction: Some(Direction::Import),
            compensated_loss: None,
            tariff_changes: Vec::new(),
        }
    }

    fn tariff() -> Tariff {
        Tariff::simple(
            "ad-hoc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19"))],
        )
    }

    #[test]
    fn a_cdr_carries_its_arithmetic() {
        let cdr = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .build()
            .unwrap();

        assert_eq!(cdr.periods.len(), 2);
        assert_eq!(cdr.total_energy.to_string(), "18.000 kWh");
        assert!(cdr.conserves());
        assert!(cdr.fully_measured());
        assert_eq!(cdr.duration(), time::Duration::minutes(30));
        assert_eq!(cdr.key.to_string(), "DE*ABC/cdr-1");
    }

    #[test]
    fn a_cdr_carries_its_money_and_it_comes_from_its_own_periods() {
        let cdr = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .evidence(evidence(IdentificationStrength::Trusted))
            .rated_with(&tariff())
            .build()
            .unwrap();

        let cost = cdr.cost.as_ref().expect("the CDR was rated");
        assert_eq!(cost.tariff_id.as_str(), "ad-hoc");
        assert_eq!(
            cost.rated.quantity_for(Dimension::Energy),
            cdr.total_energy.kwh(),
            "every kilowatt-hour priced is a kilowatt-hour the record claims"
        );
        // 18.000 kWh at 0.49, gross, with 19 % inside it.
        assert_eq!(cdr.total_cost().unwrap().to_string(), "8.82 EUR");
        assert_eq!(cost.rated.net().to_string(), "7.41 EUR");
        assert_eq!(cost.rated.tax().to_string(), "1.41 EUR");
    }

    #[test]
    fn an_unrated_cdr_has_no_price_rather_than_a_zero_one() {
        let cdr = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .build()
            .unwrap();
        assert!(cdr.cost.is_none());
        assert!(
            cdr.total_cost().is_none(),
            "a record nobody priced costs nothing known, not nothing"
        );
    }

    #[test]
    fn a_period_never_starts_before_the_session_it_belongs_to() {
        // The settlement slot is 10:00; the session began at 10:07. Both facts
        // are true and they are different fields.
        let mut s = Session::open(
            "s-2".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(7),
        );
        s.transition_to(emob_session::SessionState::Charging, at(7))
            .unwrap();
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(7),
                        kwh("100"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        at(23),
                        kwh("108"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        s.end(at(23), EndReason::Local).unwrap();

        let cdr = CdrBuilder::from_session(&s, Direction::Import)
            .unwrap()
            .key(party(), "cdr-2".parse().unwrap())
            .build()
            .unwrap();

        assert_eq!(cdr.periods[0].quarter_hour.start(), at(0));
        assert_eq!(cdr.periods[0].start, at(7), "the session began at 10:07");
        assert_eq!(cdr.periods[0].end, at(15));
        assert_eq!(cdr.periods[1].quarter_hour.start(), at(15));
        assert_eq!(cdr.periods[1].start, at(15));
        assert_eq!(cdr.periods[1].end, at(23), "and ended at 10:23");
        assert_eq!(cdr.periods[0].duration(), time::Duration::minutes(8));
    }

    #[test]
    fn a_running_session_cannot_produce_a_cdr() {
        let mut s = ended_session();
        s.ended_at = None;
        assert!(matches!(
            CdrBuilder::from_session(&s, Direction::Import),
            Err(CdrError::SessionNotEnded)
        ));
    }

    #[test]
    fn a_cdr_needs_a_key() {
        assert!(matches!(
            CdrBuilder::from_session(&ended_session(), Direction::Import)
                .unwrap()
                .build(),
            Err(CdrError::NoKey)
        ));
    }

    #[test]
    fn an_overstated_authorisation_is_refused() {
        // The session says ad-hoc — a card at the point. The signed record
        // claims the assignment was established by a secure feature, which
        // ad-hoc cannot do. Two stories about one event.
        let err = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .evidence(evidence(IdentificationStrength::Secure))
            .build()
            .unwrap_err();

        assert!(matches!(err, CdrError::AuthStrengthMismatch { .. }));
        assert!(
            err.to_string().contains("claims ad-hoc authorisation"),
            "{err}"
        );
    }

    #[test]
    fn under_reporting_is_fine() {
        // A station that reports a weaker assignment than the path could
        // support is being conservative, and that is not a fault.
        let cdr = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .evidence(evidence(IdentificationStrength::Hearsay))
            .build()
            .unwrap();
        assert!(cdr.has_evidence());
    }

    #[test]
    fn a_cdr_without_evidence_says_so() {
        let cdr = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .build()
            .unwrap();
        assert!(!cdr.has_evidence());
    }

    #[test]
    fn a_correction_names_what_it_replaces() {
        let original = CdrKey {
            party: party(),
            id: "cdr-1".parse().unwrap(),
        };
        let cdr = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-2".parse().unwrap())
            .supersedes(original.clone())
            .build()
            .unwrap();
        assert_eq!(cdr.supersedes, Some(original));
    }

    #[test]
    fn the_key_is_the_pair_not_the_bare_id() {
        // Two CPOs may each have a CDR called "1".
        let a = CdrKey {
            party: PartyId::new("DE", "ABC").unwrap(),
            id: "1".parse().unwrap(),
        };
        let b = CdrKey {
            party: PartyId::new("DE", "XYZ").unwrap(),
            id: "1".parse().unwrap(),
        };
        assert_ne!(a, b);
    }

    #[test]
    fn interpolated_periods_travel_to_the_recipient() {
        let mut s = Session::open(
            "s-2".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(7),
        );
        s.transition_to(emob_session::SessionState::Charging, at(7))
            .unwrap();
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(7),
                        kwh("100"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        at(23),
                        kwh("108"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        s.end(at(23), EndReason::Local).unwrap();

        let cdr = CdrBuilder::from_session(&s, Direction::Import)
            .unwrap()
            .key(party(), "cdr-3".parse().unwrap())
            .build()
            .unwrap();

        assert!(!cdr.fully_measured());
        assert!(cdr.conserves());
        assert!(
            cdr.periods
                .iter()
                .all(|p| p.provenance == Provenance::Interpolated)
        );
    }

    #[test]
    fn the_time_after_the_last_reading_is_occupancy_and_it_gets_billed() {
        // A car finishes charging at 10:30 and is collected at 11:05. The
        // meter stopped moving; the parking space did not become free. The
        // occupancy fee of AFIR Art. 5(4) exists for exactly those 35 minutes,
        // and a record that stops at the last reading cannot bill them.
        let mut s = ended_session();
        s.ended_at = Some(at(65));

        let occupancy = Tariff::simple(
            "ad-hoc-dc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![
                PriceComponent::new(Dimension::Energy, dec("0.49")),
                PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
            ],
        );

        let cdr = CdrBuilder::from_session(&s, Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .rated_with(&occupancy)
            .build()
            .unwrap();

        // Two metered quarter hours, then 10:30–10:45, 10:45–11:00, 11:00–11:05.
        assert_eq!(cdr.periods.len(), 5, "{:?}", cdr.periods);
        assert_eq!(cdr.periods.last().unwrap().end, at(65));
        assert!(
            cdr.periods[2..].iter().all(|p| p.energy.is_zero()),
            "no energy is invented for the time nobody measured"
        );
        assert!(cdr.conserves(), "and the total is untouched");
        assert_eq!(cdr.total_energy.to_string(), "18.000 kWh");

        let rated = &cdr.cost.as_ref().unwrap().rated;
        assert_eq!(rated.amount_for(Dimension::Energy), Some(dec("8.82000")));
        // 35 minutes at 6.00 per hour.
        assert_eq!(rated.amount_for(Dimension::ParkingTime), Some(dec("3.50")));
    }

    #[test]
    fn a_taper_is_not_an_occupancy_even_when_it_moved_nothing() {
        // The distinction a guess from `energy == 0` gets wrong. A car at
        // 100 % state of charge can leave a quarter hour at exactly 0.000 kWh
        // while the session's own state machine still says Charging — and
        // billing that quarter hour at the occupancy rate charges a driver for
        // parking they were told was charging.
        let mut s = Session::open(
            "s-taper".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(0),
        );
        s.transition_to(emob_session::SessionState::Charging, at(0))
            .unwrap();
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        at(15),
                        kwh("110.000"),
                        Direction::Import,
                        ReadingContext::SampleClock,
                    ),
                    // The taper: fifteen minutes, nothing delivered.
                    MeterReading::new(
                        at(30),
                        kwh("110.000"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        s.end(at(30), EndReason::Local).unwrap();

        let occupancy = Tariff::simple(
            "ad-hoc-dc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![
                PriceComponent::new(Dimension::Energy, dec("0.49")),
                PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
            ],
        );

        let cdr = CdrBuilder::from_session(&s, Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .rated_with(&occupancy)
            .build()
            .unwrap();

        assert!(cdr.periods[1].energy.is_zero());
        assert!(
            cdr.periods[1].activity == Activity::Charging,
            "the session says it was charging, and the session is the record of that"
        );
        assert_eq!(
            cdr.cost
                .as_ref()
                .unwrap()
                .rated
                .amount_for(Dimension::ParkingTime),
            None,
            "no occupancy fee for a taper"
        );
    }

    #[test]
    fn a_suspended_slot_is_priced_as_occupancy_and_says_so() {
        // The mirror image, and the reason the flag is read rather than
        // derived: the same zero-energy quarter hour, with the session's own
        // history saying the vehicle was connected and not charging.
        let mut s = Session::open(
            "s-parked".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(0),
        );
        s.transition_to(emob_session::SessionState::Charging, at(0))
            .unwrap();
        s.transition_to(emob_session::SessionState::SuspendedByVehicle, at(15))
            .unwrap();
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        at(15),
                        kwh("110.000"),
                        Direction::Import,
                        ReadingContext::SampleClock,
                    ),
                    MeterReading::new(
                        at(30),
                        kwh("110.000"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        s.end(at(30), EndReason::Local).unwrap();

        let occupancy = Tariff::simple(
            "ad-hoc-dc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![
                PriceComponent::new(Dimension::Energy, dec("0.49")),
                PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
            ],
        );

        let cdr = CdrBuilder::from_session(&s, Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .rated_with(&occupancy)
            .build()
            .unwrap();

        assert_eq!(cdr.periods[1].activity, Activity::Parked);
        // Fifteen minutes at 6.00 an hour.
        assert_eq!(
            cdr.cost
                .as_ref()
                .unwrap()
                .rated
                .amount_for(Dimension::ParkingTime),
            Some(dec("1.50"))
        );
    }

    #[test]
    fn occupancy_is_measured_from_when_charging_stopped_not_from_the_next_quarter() {
        // The fault this fixes. `[AFIR Art. 5(4)]` prices the time a vehicle is
        // connected and not charging *per minute*, and a vehicle stops charging
        // when it stops charging rather than at `:15:00`. Split on the
        // settlement grid alone, the quarter hour a charge finishes in carries
        // one `charging` flag for fifteen minutes that were not all the same —
        // so the ten minutes of occupancy between 10:20 and 10:30 were billed
        // as charging, and the driver paid 1.50 for 25 minutes of parking
        // instead of 2.50.
        let mut s = Session::open(
            "s-taper".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(0),
        );
        s.transition_to(emob_session::SessionState::Charging, at(0))
            .unwrap();
        s.transition_to(emob_session::SessionState::SuspendedByVehicle, at(20))
            .unwrap();
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        at(20),
                        kwh("113.000"),
                        Direction::Import,
                        ReadingContext::InterruptionBegin,
                    ),
                    MeterReading::new(
                        at(45),
                        kwh("113.000"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        s.end(at(45), EndReason::Local).unwrap();

        let occupancy = Tariff::simple(
            "ad-hoc-dc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![
                PriceComponent::new(Dimension::Energy, dec("0.49")),
                PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
            ],
        );

        let cdr = CdrBuilder::from_session(&s, Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .rated_with(&occupancy)
            .build()
            .unwrap();

        let parked: i64 = cdr
            .periods
            .iter()
            .filter(|p| p.activity != Activity::Charging)
            .map(|p| p.duration().whole_minutes())
            .sum();
        let charged: i64 = cdr
            .periods
            .iter()
            .filter(|p| p.activity == Activity::Charging)
            .map(|p| p.duration().whole_minutes())
            .sum();
        assert_eq!((charged, parked), (20, 25));

        let rated = &cdr.cost.as_ref().unwrap().rated;
        // Twenty-five minutes at 6.00 an hour, exactly — and the amount comes
        // from the seconds, not from 25/60 of an hour.
        assert_eq!(rated.amount_for(Dimension::ParkingTime), Some(dec("2.50")));
        assert_eq!(rated.base_quantity_for(Dimension::ParkingTime), dec("1500"));
        assert!(rated.lines_reconcile());
        assert!(cdr.conserves());

        // …and the market side still sees one entry per Messperiode, whatever
        // the pricing cut did inside them `[A6 §IV.1]`.
        let market = s.split(Direction::Import).unwrap().market_series();
        assert_eq!(market.len(), 3, "10:15, 10:30, 10:45");
    }

    #[test]
    fn a_gap_the_session_calls_charging_is_not_billed_as_occupancy() {
        // The mirror of `a_taper_is_not_an_occupancy`, one field along. A
        // station that goes quiet — a dropped `MeterValues`, a WAN outage —
        // while its own session state machine still says `Charging` leaves a
        // stretch of the window nothing measured. Reading that silence as
        // "connected and not charging" bills the driver an occupancy fee per
        // minute `[AFIR Art. 5(4)]` for time the operator's own record says the
        // car was taking energy, and no field on the record would show it.
        //
        // The flag is a stated fact everywhere else on this type. It is one
        // here too.
        let mut s = Session::open(
            "s-quiet".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(0),
        );
        s.transition_to(emob_session::SessionState::Charging, at(0))
            .unwrap();
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        at(30),
                        kwh("110.000"),
                        Direction::Import,
                        ReadingContext::SampleClock,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        // Suspended at 10:50, twenty minutes after the meter last spoke, and
        // collected at 11:05.
        s.transition_to(emob_session::SessionState::SuspendedByVehicle, at(50))
            .unwrap();
        s.end(at(65), EndReason::Local).unwrap();

        let occupancy = Tariff::simple(
            "ad-hoc-dc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![
                PriceComponent::new(Dimension::Energy, dec("0.49")),
                PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
            ],
        );

        let cdr = CdrBuilder::from_session(&s, Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .rated_with(&occupancy)
            .build()
            .unwrap();

        // 10:30–10:45 and 10:45–10:50 are unmetered and the session says
        // charging; 10:50–11:00 and 11:00–11:05 are unmetered and suspended.
        let unmetered: Vec<(i64, i64, bool)> = cdr
            .periods
            .iter()
            .filter(|p| p.start >= at(30))
            .map(|p| {
                (
                    (p.start - at(0)).whole_minutes(),
                    (p.end - at(0)).whole_minutes(),
                    p.activity == Activity::Charging,
                )
            })
            .collect();
        assert_eq!(
            unmetered,
            vec![
                (30, 45, true),
                (45, 50, true),
                (50, 60, false),
                (60, 65, false)
            ],
            "the cut lands on the state change as well as on the grid"
        );

        // Fifteen minutes of occupancy, not thirty-five: the twenty minutes the
        // session called charging are not a fee.
        let rated = &cdr.cost.as_ref().unwrap().rated;
        assert_eq!(rated.base_quantity_for(Dimension::ParkingTime), dec("900"));
        assert_eq!(rated.amount_for(Dimension::ParkingTime), Some(dec("1.50")));
        assert!(cdr.conserves());
        assert!(
            crate::validate(&cdr).is_settleable(),
            "{:?}",
            crate::validate(&cdr).findings
        );
    }

    #[test]
    fn a_session_wider_than_its_readings_claims_no_measurement_it_does_not_have() {
        // The station authorises at 10:00 and sends its first meter value at
        // 10:20. Clamping the settlement slot to the session window would make
        // the first period claim 10:15–10:30 for energy that was only measured
        // from 10:20, and would leave the twenty minutes before it unbilled as
        // occupancy. The slot carries its own window instead.
        //
        // The session stays `Pending` until the transaction's register opens,
        // which is what "authorised, and no energy has moved" means — so those
        // twenty minutes are connected-and-not-charging by the operator's own
        // record rather than by the meter's silence.
        let mut s = Session::open(
            "s-late".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(0),
        );
        s.transition_to(emob_session::SessionState::Charging, at(20))
            .unwrap();
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(20),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        at(50),
                        kwh("110.000"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        s.end(at(50), EndReason::Local).unwrap();

        let cdr = CdrBuilder::from_session(&s, Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .build()
            .unwrap();

        // 10:00–10:15 and 10:15–10:20 are occupancy; the metered periods start
        // at 10:20 exactly.
        assert_eq!(cdr.periods[0].start, at(0));
        assert!(
            cdr.periods
                .iter()
                .take(2)
                .all(|p| p.activity != Activity::Charging)
        );
        let first_metered = cdr
            .periods
            .iter()
            .find(|p| !p.energy.is_zero())
            .expect("a metered period");
        assert_eq!(
            first_metered.start,
            at(20),
            "no period claims energy it was not measured for"
        );
        assert_eq!(cdr.periods.first().unwrap().start, cdr.started_at);
        assert_eq!(cdr.periods.last().unwrap().end, cdr.ended_at);
        assert!(cdr.conserves());
        assert!(
            crate::validate(&cdr).is_settleable(),
            "{:?}",
            crate::validate(&cdr).findings
        );
    }

    #[test]
    fn energy_in_a_slot_the_session_calls_suspended_is_refused() {
        // The meter and the state machine are two records of one event. When
        // they disagree, a CDR that picks one silently is a CDR that bills a
        // charge the operator's own log says never happened.
        let mut s = Session::open(
            "s-3".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(0),
        );
        s.transition_to(emob_session::SessionState::Charging, at(0))
            .unwrap();
        s.transition_to(emob_session::SessionState::SuspendedByVehicle, at(15))
            .unwrap();
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    // The register had not moved when the session paused…
                    MeterReading::new(
                        at(15),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::SampleClock,
                    ),
                    // …and yet ten kilowatt-hours crossed the meter between
                    // 10:15 and 10:30, measured at both ends. There is no
                    // charging time to attribute them to.
                    MeterReading::new(
                        at(30),
                        kwh("110.000"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        s.end(at(30), EndReason::Local).unwrap();

        let err = CdrBuilder::from_session(&s, Direction::Import).unwrap_err();
        assert!(
            matches!(err, CdrError::EnergyWhileNotCharging { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("records as suspended"), "{err}");
    }

    #[test]
    fn the_ordinary_ocpp_201_shape_builds_and_prices_the_wait_as_occupancy() {
        // `TransactionEvent(Started)` with `chargingState = EVConnected` and a
        // `Transaction.Begin` reading; `Charging` thirty seconds later with no
        // reading; `Sample.Clock` at the quarter hour; `Ended` at 10:40. The
        // most common transaction shape on a 2.0.1 estate — and, drawn as one
        // straight line between the readings, one that was refused for a
        // contradiction the interpolation itself had invented.
        let charging_from = at(0) + time::Duration::seconds(30);
        let mut s = Session::open(
            "s-201".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(0),
        );
        s.transition_to(emob_session::SessionState::SuspendedByVehicle, at(0))
            .unwrap();
        s.transition_to(emob_session::SessionState::Charging, charging_from)
            .unwrap();
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        at(15),
                        kwh("105.000"),
                        Direction::Import,
                        ReadingContext::SampleClock,
                    ),
                    MeterReading::new(
                        at(40),
                        kwh("115.000"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        s.end(at(40), EndReason::Local).unwrap();

        let occupancy = Tariff::simple(
            "ad-hoc-dc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![
                PriceComponent::new(Dimension::Energy, dec("0.49")),
                PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
            ],
        );
        let cdr = CdrBuilder::from_session(&s, Direction::Import)
            .unwrap()
            .key(party(), "cdr-201".parse().unwrap())
            .rated_with(&occupancy)
            .build()
            .expect("the ordinary 2.0.1 transaction shape builds");

        // The thirty seconds before the charge began moved nothing and were
        // not charging: occupancy, not charging time, and no invented energy.
        let wait = &cdr.periods[0];
        assert_eq!((wait.start, wait.end), (at(0), charging_from));
        assert!(wait.energy.is_zero());
        assert_eq!(
            wait.activity,
            Activity::Parked,
            "EVConnected is connected and not charging"
        );
        assert_eq!(cdr.periods[1].activity, Activity::Charging);
        assert!(cdr.conserves());
        assert_eq!(cdr.total_energy.to_string(), "15.000 kWh");

        let rated = &cdr.cost.as_ref().unwrap().rated;
        assert_eq!(rated.amount_for(Dimension::Energy), Some(dec("7.35000")));
        // Thirty seconds is below the sixty the regulation's cap resolves, so
        // the occupancy line goes and the record says why `[REA 6-A §3.1]` —
        // rather than the kilowatt-hours going with it.
        assert_eq!(rated.amount_for(Dimension::ParkingTime), None);
        assert!(rated.reasons().any(|r| r.contains("REA 6-A")));
        assert!(crate::validate(&cdr).is_settleable());

        // A station whose type approval states a ten-second clock bills it:
        // thirty seconds at 6.00 an hour.
        let precise = ClockResolution::stated(time::Duration::seconds(10)).unwrap();
        let cdr = CdrBuilder::from_session(&s, Direction::Import)
            .unwrap()
            .key(party(), "cdr-201".parse().unwrap())
            .clock(precise)
            .rated_with(&occupancy)
            .build()
            .unwrap();
        let rated = &cdr.cost.as_ref().unwrap().rated;
        assert_eq!(rated.amount_for(Dimension::ParkingTime), Some(dec("0.05")));
    }

    #[test]
    fn readings_that_outlast_the_session_are_refused_rather_than_clamped() {
        // OCPP delivers `MeterValues` asynchronously, so a `StopTransaction`
        // timestamp preceding the last reading is ordinary. A period runs over
        // the part of its slot the readings covered, so the record would claim
        // measurement after its own end — which `validate` blocks on the way in
        // and the builder was happy to emit on the way out. Every unit test
        // passed; the two halves of the crate disagreed.
        let mut s = Session::open(
            "s-late".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(0),
        );
        s.transition_to(emob_session::SessionState::Charging, at(0))
            .unwrap();
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        at(60),
                        kwh("118.000"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        // …and the transaction closed half an hour before the last reading.
        s.end(at(30), EndReason::Local).unwrap();

        let err = CdrBuilder::from_session(&s, Direction::Import).unwrap_err();
        assert!(
            matches!(err, CdrError::ReadingsOutsideSession { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("would invent one"));

        // The mirror image: a reading from before the session opened.
        let mut early = Session::open(
            "s-early".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(10),
        );
        early
            .transition_to(emob_session::SessionState::Charging, at(10))
            .unwrap();
        early
            .attach_series(
                MeterSeries::new(
                    Direction::Import,
                    vec![
                        MeterReading::new(
                            at(0),
                            kwh("100.000"),
                            Direction::Import,
                            ReadingContext::TransactionBegin,
                        ),
                        MeterReading::new(
                            at(30),
                            kwh("118.000"),
                            Direction::Import,
                            ReadingContext::TransactionEnd,
                        ),
                    ],
                )
                .unwrap(),
            )
            .unwrap();
        early.end(at(30), EndReason::Local).unwrap();

        assert!(matches!(
            CdrBuilder::from_session(&early, Direction::Import),
            Err(CdrError::ReadingsOutsideSession { .. })
        ));
    }

    #[test]
    fn every_record_this_builder_emits_passes_its_own_validator() {
        // The property the two halves of this crate owe each other, and the one
        // that was quietly false. A builder that emits what its validator
        // blocks is two rules about one record.
        let cdr = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .build()
            .unwrap();

        let report = crate::validate(&cdr);
        assert!(
            report
                .blocking()
                .all(|f| matches!(f, crate::Finding::EnergyNotBillable)),
            "the builder emitted a record it would refuse: {:?}",
            report.blocking().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_draw_over_an_export_register_is_refused() {
        // The invariant the workspace claims everywhere and could not check
        // until the OBIS code was read: `[OCMF Tab. 25]` reserves `C*` for
        // export, so a record claiming a draw over one is a V2G discharge
        // billed as consumption.
        let mut discharge = evidence(IdentificationStrength::Trusted);
        discharge.direction = Some(Direction::Export);

        let err = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .evidence(discharge)
            .build()
            .unwrap_err();
        assert!(matches!(err, CdrError::DirectionMismatch { .. }), "{err}");
        assert!(err.to_string().contains("never net"));
    }

    #[test]
    fn a_register_the_verifier_could_not_classify_makes_no_claim() {
        // Not import by default: a record whose OBIS code the verifier does not
        // recognise simply does not state a direction, and the CDR is free to.
        let mut unclassified = evidence(IdentificationStrength::Trusted);
        unclassified.direction = None;

        assert!(
            CdrBuilder::from_session(&ended_session(), Direction::Import)
                .unwrap()
                .key(party(), "cdr-1".parse().unwrap())
                .evidence(unclassified)
                .build()
                .is_ok()
        );
    }

    #[test]
    fn a_time_priced_tariff_is_refused_when_the_clock_cannot_be_defended() {
        // The payoff of keeping the two quantities apart. `[OCMF Tab. 19]` says
        // how far the station's clock can be trusted; a per-minute occupancy fee
        // `[AFIR Art. 5(4)]` bills a duration. Billing one off the other is a
        // number nobody can defend, and no platform in the field checks it.
        let mut unsynchronised = evidence(IdentificationStrength::Trusted);
        unsynchronised.duration_billable = false;

        let occupancy = Tariff::simple(
            "ad-hoc-dc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![
                PriceComponent::new(Dimension::Energy, dec("0.49")),
                PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
            ],
        );

        let err = CdrBuilder::from_session(&occupied_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .evidence(unsynchronised.clone())
            .rated_with(&occupancy)
            .build()
            .unwrap_err();
        assert!(matches!(err, CdrError::DurationNotBillable { .. }), "{err}");
        assert!(err.to_string().contains("price this session per kWh"));

        // …and a session that charged **no** occupancy under the very same
        // tariff is not refused. The gate asks what the record bills, not what
        // the tariff mentions: refusing here would throw away the
        // kilowatt-hours over a fee nobody was charged.
        let cdr = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .evidence(unsynchronised.clone())
            .rated_with(&occupancy)
            .build()
            .unwrap();
        assert_eq!(cdr.total_cost().unwrap().to_string(), "8.82 EUR");

        // …and the energy is genuinely unaffected: the same session on a
        // per-kWh tariff builds and prices.
        let energy_only = Tariff::simple(
            "ad-hoc-dc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(Dimension::Energy, dec("0.49"))],
        );
        let cdr = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .evidence(unsynchronised)
            .rated_with(&energy_only)
            .build()
            .unwrap();
        assert_eq!(cdr.total_cost().unwrap().to_string(), "8.82 EUR");
    }

    #[test]
    fn a_session_whose_readings_span_it_gains_no_occupancy_periods() {
        let cdr = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .build()
            .unwrap();
        assert_eq!(cdr.periods.len(), 2);
        assert_eq!(cdr.periods[0].start, cdr.started_at);
        assert_eq!(cdr.periods.last().unwrap().end, cdr.ended_at);
    }

    /// A session too short for a conforming clock to resolve.
    fn brief_session() -> Session {
        let mut s = Session::open(
            "s-brief".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(0),
        );
        s.transition_to(emob_session::SessionState::Charging, at(0))
            .unwrap();
        let thirty_seconds = at(0) + time::Duration::seconds(30);
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        thirty_seconds,
                        kwh("100.400"),
                        Direction::Import,
                        ReadingContext::TransactionEnd,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        s.end(thirty_seconds, EndReason::Local).unwrap();
        s
    }

    #[test]
    fn a_record_whose_chain_did_not_verify_cannot_be_re_rated_either() {
        // The builder refuses to *issue* such a record. `rerated_with` is the
        // other door, and it is the one an eMSP uses on a partner's document —
        // which never passed the builder at all, because `from_ocpi` assembles
        // a `Cdr` directly. Without this gate the same record that the CPO side
        // refuses to bill is priced at the EMP side's retail tariff and
        // invoiced to a driver: D70's settled record for a forged session,
        // reached through the door D206 opened (D231).
        let good = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .evidence(evidence(IdentificationStrength::Trusted))
            .build()
            .unwrap();
        assert!(
            good.cost.is_none(),
            "built unrated, as the EMP path receives it"
        );

        let mut forged = good.clone();
        forged.evidence.as_mut().unwrap().energy_billable = false;
        let err = forged.rerated_with(&tariff()).unwrap_err();
        assert!(matches!(err, CdrError::EnergyNotBillable), "{err}");

        // …and the record whose chain *did* hold up re-rates as it always has,
        // so the gate refuses the forged session rather than the door.
        assert!(good.rerated_with(&tariff()).is_ok());

        // Evidence that is absent is a different question — which regime a
        // record with no signature falls under is the billing layer's — and it
        // still prices, as `validate` and the builder both already say.
        let mut unsigned = good.clone();
        unsigned.evidence = None;
        assert!(unsigned.rerated_with(&tariff()).is_ok());
    }

    #[test]
    fn evidence_that_is_present_and_failed_is_worse_than_evidence_that_is_absent() {
        // The workspace's central promise, at the one seam where it was not
        // being kept: a CDR's energy comes from the session's meter series, so
        // without this check a record would be priced off a register nothing
        // verified while carrying an `EvidenceRef` that made it look checked.
        let mut failed = evidence(IdentificationStrength::Trusted);
        failed.energy_billable = false;

        let err = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .evidence(failed)
            .rated_with(&tariff())
            .build()
            .unwrap_err();

        assert!(matches!(err, CdrError::EnergyNotBillable), "{err}");
        assert!(err.to_string().contains("does not verify does not bill"));

        // …and a record with no evidence at all still builds, because that is a
        // question for the billing layer that knows which regime applies.
        assert!(
            CdrBuilder::from_session(&ended_session(), Direction::Import)
                .unwrap()
                .key(party(), "cdr-1".parse().unwrap())
                .rated_with(&tariff())
                .build()
                .is_ok()
        );
    }

    #[test]
    fn a_span_the_clock_cannot_resolve_is_not_billed_for_time() {
        // `[REA 6-A §3.1]`: "Messwerte unterhalb der kürzest möglichen
        // Zeitspanne werden nicht für Abrechnungszwecke verwendet." A thirty-
        // second session billed per minute is billing a number the clock
        // cannot defend — the mirror of an unsynchronised clock, arriving from
        // the resolution end rather than the placement end.
        let by_the_minute = Tariff::simple(
            "ad-hoc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(Dimension::Time, dec("6.00"))],
        );

        let cdr = CdrBuilder::from_session(&brief_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .rated_with(&by_the_minute)
            .build()
            .expect("the record builds; the line it may not bill is dropped");

        let rated = &cdr.cost.as_ref().unwrap().rated;
        assert_eq!(rated.amount_for(Dimension::Time), None);
        assert_eq!(cdr.total_cost().unwrap().to_string(), "0.00 EUR");
        assert!(
            rated.reasons().any(|r| r.contains("REA 6-A")),
            "{:?}",
            rated.notes
        );

        // …and the energy is genuinely unaffected.
        let per_kwh = Tariff::simple(
            "ad-hoc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(Dimension::Energy, dec("0.49"))],
        );
        let cdr = CdrBuilder::from_session(&brief_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .rated_with(&per_kwh)
            .build()
            .unwrap();
        assert_eq!(cdr.total_cost().unwrap().to_string(), "0.20 EUR");
    }

    #[test]
    fn a_station_whose_clock_is_better_than_the_worst_case_may_say_so() {
        // The default is the regulation's cap, because a platform that has not
        // read the type approval has not been told the device is better than
        // the worst case it permits. One that has, says so.
        let by_the_minute = Tariff::simple(
            "ad-hoc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(Dimension::Time, dec("6.00"))],
        );
        let precise = emob_core::ClockResolution::stated(time::Duration::seconds(10)).unwrap();

        let cdr = CdrBuilder::from_session(&brief_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .clock(precise)
            .rated_with(&by_the_minute)
            .build()
            .unwrap();

        // Thirty seconds at 6.00 an hour.
        assert_eq!(cdr.total_cost().unwrap().to_string(), "0.05 EUR");
    }

    #[test]
    fn a_signed_tariff_change_inside_the_session_is_refused() {
        // `[OCMF Tab. 7, TX=T]` is the station's own record of where its price
        // changed. A CDR is priced by the one version in force when the session
        // started `[AFIR Art. 5(4)]`, so a change signed inside it is the meter
        // saying two prices applied to a record that states one — and billing
        // either of them is picking a number over a signed statement that
        // contradicts it. The instants were read off the chain and dropped at
        // this seam until now, which is a rule modelled and never consulted.
        let mut signed = evidence(IdentificationStrength::Trusted);
        signed.tariff_changes = vec![at(15)];

        let err = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .evidence(signed.clone())
            .rated_with(&tariff())
            .build()
            .unwrap_err();
        assert!(
            matches!(err, CdrError::SignedTariffChangeInsideSession { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("TX=T"), "{err}");
        assert!(
            !err.to_string().contains("settlement-period boundary"),
            "10:15 is a boundary, so the metrology rule is not the objection: {err}"
        );

        // …and one that lands mid-period says that too `[PTB-A 50.7 §3.1.7.2]`.
        let mut off_grid = evidence(IdentificationStrength::Trusted);
        off_grid.tariff_changes = vec![at(20)];
        let err = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .evidence(off_grid)
            .rated_with(&tariff())
            .build()
            .unwrap_err();
        assert!(err.to_string().contains("PTB-A 50.7"), "{err}");

        // A change at the session's own edges is not inside it: a version that
        // takes effect exactly when the session ends priced none of it.
        let mut edges = evidence(IdentificationStrength::Trusted);
        edges.tariff_changes = vec![at(0), at(30)];
        assert!(
            CdrBuilder::from_session(&ended_session(), Direction::Import)
                .unwrap()
                .key(party(), "cdr-1".parse().unwrap())
                .evidence(edges)
                .rated_with(&tariff())
                .build()
                .is_ok()
        );

        // …and an unrated record is not priced at all, so there is nothing for
        // the marker to contradict.
        assert!(
            CdrBuilder::from_session(&ended_session(), Direction::Import)
                .unwrap()
                .key(party(), "cdr-1".parse().unwrap())
                .evidence(signed)
                .build()
                .is_ok()
        );
    }

    #[test]
    fn the_cable_loss_the_meter_compensated_reaches_the_record() {
        // `[OCMF Tab. 7, CL]` states how much of the register is cable rather
        // than vehicle. The chain has computed it since the register was read
        // and it stopped at the crate that computed it — so a partner disputing
        // the energy, and a customer `[REA 6-A §3.2]` entitled to know what is
        // inside a measured value, could not be told.
        let mut with_loss = evidence(IdentificationStrength::Trusted);
        with_loss.compensated_loss = Some(kwh("0.150"));

        let cdr = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .evidence(with_loss)
            .rated_with(&tariff())
            .build()
            .unwrap();

        assert_eq!(
            cdr.evidence.as_ref().unwrap().compensated_loss,
            Some(kwh("0.150"))
        );
        // Nothing is subtracted: the compensation is already inside the
        // register the session billed.
        assert_eq!(cdr.total_energy.to_string(), "18.000 kWh");
        assert!(crate::validate(&cdr).is_settleable());
    }

    #[test]
    fn a_record_names_the_tariff_that_priced_it_by_content() {
        // A tariff id is a name and names get reused. The fingerprint is what
        // lets a receiving party ask "is the tariff I hold the one that
        // produced these euros" *before* it compares totals and finds them
        // different.
        let priced = tariff();
        let cdr = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .rated_with(&priced)
            .build()
            .unwrap();

        assert!(cdr.was_priced_with(&priced));

        // The same id, one price edited in place — the case an id cannot see.
        let mut edited = tariff();
        edited.elements[0].components[0].price = dec("0.59");
        assert_eq!(edited.id, priced.id);
        assert!(
            !cdr.was_priced_with(&edited),
            "an in-place edit keeps the id and is a different tariff"
        );

        let cost = cdr.cost.as_ref().unwrap();
        assert_eq!(cost.tariff_fingerprint, priced.fingerprint());
        assert_eq!(cost.tariff_fingerprint.to_string().len(), 64);
    }

    #[test]
    fn the_version_in_force_when_the_session_started_is_the_one_that_governs() {
        // `[AFIR Art. 5(4)]`: the price must be known to the driver "before
        // they initiate a recharging session", so a change mid-session does
        // not reach back into it.
        let session = ended_session();
        let old = tariff().valid_between(None, Some(at(15)));
        let new_ = {
            let mut t = tariff();
            t.elements[0].components[0].price = dec("0.99");
            t.valid_between(Some(at(15)), None)
        };
        let history = emob_tariff::TariffHistory::new(vec![old, new_]).unwrap();

        // The session runs 10:00–10:30 and the price changed at 10:15; the
        // tariff in force at 10:00 is the one the driver was shown.
        let cdr = CdrBuilder::from_session(&session, Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .rated_with_history(&history)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(cdr.total_cost().unwrap().to_string(), "8.82 EUR");
    }

    #[test]
    fn a_tariff_that_was_not_in_force_cannot_have_priced_the_session() {
        let future = tariff().valid_between(Some(at(600)), None);
        let err = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .rated_with(&future)
            .build()
            .unwrap_err();

        assert!(matches!(err, CdrError::TariffNotInForce { .. }), "{err}");
        assert!(err.to_string().contains("before the session starts"));
    }

    #[test]
    fn a_gap_in_a_price_history_is_reported_rather_than_guessed() {
        let history = emob_tariff::TariffHistory::new(vec![
            tariff().valid_between(None, Some(at(-60))),
            tariff().valid_between(Some(at(600)), None),
        ])
        .unwrap();

        let err = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .rated_with_history(&history)
            .unwrap_err();

        assert!(matches!(err, CdrError::NoTariffInForce { .. }), "{err}");
        assert!(err.to_string().contains("guessing a price is not a fix"));
    }

    #[test]
    fn a_tiered_tariff_prices_a_cdr_slot_by_slot() {
        // The reason the CDR hands its own periods to the rating: the quarter
        // hours that conserve energy are the periods that carry the tiers.
        let tiered = Tariff {
            id: "tiered".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            tax_included: emob_tariff::TaxIncluded::Yes,
            elements: vec![
                emob_tariff::TariffElement {
                    components: vec![PriceComponent::new(Dimension::Energy, dec("0.39"))],
                    restrictions: emob_tariff::Restrictions {
                        max_kwh: Some(dec("10")),
                        ..emob_tariff::Restrictions::default()
                    },
                },
                emob_tariff::TariffElement::unrestricted(vec![PriceComponent::new(
                    Dimension::Energy,
                    dec("0.59"),
                )]),
            ],
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        };

        let cdr = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .rated_with(&tiered)
            .build()
            .unwrap();

        let rated = &cdr.cost.as_ref().unwrap().rated;
        assert_eq!(rated.lines.len(), 2, "{:?}", rated.lines);
        // The 10:00 slot moved 10 kWh at 0.39; the 10:15 slot the other 8 at
        // 0.59, because by then ten had already been delivered.
        assert_eq!(rated.lines[0].quantity, dec("10.000"));
        assert_eq!(rated.lines[0].unit_price, dec("0.39"));
        assert_eq!(rated.lines[1].quantity, dec("8.000"));
        assert_eq!(rated.lines[1].unit_price, dec("0.59"));
        assert_eq!(
            rated.quantity_for(Dimension::Energy),
            cdr.total_energy.kwh()
        );
    }
}
