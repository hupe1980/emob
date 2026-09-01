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
    CdrId, ClockResolution, Direction, Energy, EvseId, IdentificationStrength, PartyId, SessionId,
    TariffId,
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
    /// Whether the session was delivering energy during this period.
    ///
    /// A **stated fact, not a derived one**, and the distinction decides money:
    /// `[AFIR Art. 5(4)]` lets a fast charger add an occupancy fee per minute
    /// for the time a vehicle is connected and *not* charging, so a period on
    /// the wrong side of this flag is billed at the wrong rate.
    ///
    /// Deriving it from `energy == 0` — which this type used to do — gets a
    /// taper wrong in exactly the case that matters. A car at 100 % state of
    /// charge draws a rounding error, and a quarter hour that genuinely
    /// measured `0.000 kWh` while the session's own state machine says
    /// `Charging` is a taper, not an occupancy. [`CdrBuilder`] takes this from
    /// the session history instead.
    pub charging: bool,
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
    /// The periods, in time order, summing to [`Self::total_energy`].
    pub periods: Vec<ChargingPeriod>,
    /// The total. Equal to the sum of the periods, checked at construction.
    pub total_energy: Energy,
    /// Which way the energy flowed.
    pub direction: Direction,
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

    /// Whether every period's energy was measured rather than interpolated.
    #[must_use]
    pub fn fully_measured(&self) -> bool {
        self.periods
            .iter()
            .all(|p| p.provenance == Provenance::Measured)
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
        self.cost.as_ref().map(|c| c.rated.gross())
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

    /// The session, in the terms a tariff prices it.
    ///
    /// The bridge between the settlement grid and the price: a CDR's periods
    /// *are* the rating periods, so a re-rating by the receiving party reads
    /// exactly the same slices the issuer did.
    ///
    /// # Errors
    ///
    /// [`CdrError::NotChargeable`] when the periods cannot form a session — an
    /// empty record, or one whose periods overlap.
    pub fn chargeable(&self) -> Result<Chargeable, CdrError> {
        let periods = self
            .periods
            .iter()
            .map(|p| Period {
                start: p.start,
                end: p.end,
                energy: p.energy,
                // Read, not re-derived. The record states whether the session
                // was charging; inferring it from a zero energy would bill a
                // taper at the occupancy rate.
                charging: p.charging,
            })
            .collect();
        Chargeable::new(periods).map_err(CdrError::NotChargeable)
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
    split: SessionSplit,
    /// Whether the session was charging in each slot, in the split's order —
    /// read off the session's state machine rather than guessed from the
    /// energy.
    charging: Vec<bool>,
    clock: ClockResolution,
    evidence: Option<EvidenceRef>,
    tariff: Option<&'a Tariff>,
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
    /// the session does not, or [`CdrError::EnergyWhileSuspended`] when a slot
    /// moved energy the session says it was not charging in.
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
        // how much moved. When they disagree — energy across a slot the session
        // was suspended for from end to end — one of the two is wrong, and
        // guessing which is how a driver is billed for a charge the operator's
        // own records say never happened.
        //
        // The same question, asked of the slot's *measured* window rather than
        // of the whole quarter hour, also decides whether the period is priced
        // as charging or as occupancy — so it is answered once, here, and
        // carried on the record.
        let mut charging = Vec::with_capacity(split.slots.len());
        for slot in &split.slots {
            let suspended = session.suspended_throughout(slot.from, slot.to);
            if suspended && !slot.energy.is_zero() {
                return Err(CdrError::EnergyWhileSuspended {
                    at: slot.quarter_hour.start(),
                    energy: slot.energy,
                });
            }
            charging.push(!suspended);
        }

        Ok(Self {
            key: None,
            session_id: session.id.clone(),
            evse_id: session.evse_id.clone(),
            started_at: session.started_at,
            ended_at,
            auth_path: session.authorization.path,
            split,
            charging,
            clock: ClockResolution::default(),
            evidence: None,
            tariff: None,
            supersedes: None,
        })
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
    /// told the device is better than that.
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
        let charging = &self.charging;
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
                charging: charging.get(index).copied().unwrap_or(true),
                provenance: slot.provenance,
            })
            .collect();

        // The meter series spans the readings; the session spans the parking
        // space. A car that finishes charging at 11:00 and is collected at
        // 13:00 leaves two hours the split knows nothing about — and those two
        // hours are exactly what `[AFIR Art. 5(4)]`'s occupancy fee is for. A
        // record that stops at the last reading cannot bill them, and a record
        // that stretches the last reading over them bills energy that did not
        // flow. So the gap is filled with periods carrying no energy, marked
        // interpolated because the zero is an assumption rather than a
        // measurement.
        let metered_from = periods.first().map_or(ended_at, |p| p.start);
        let metered_to = periods.last().map_or(started_at, |p| p.end);
        let mut occupancy = occupancy_periods(started_at, metered_from);
        occupancy.extend(occupancy_periods(metered_to, ended_at));
        periods.extend(occupancy);
        periods.sort_by_key(|p| p.start);

        let mut cdr = Cdr {
            key,
            session_id: self.session_id,
            evse_id: self.evse_id,
            started_at,
            ended_at,
            auth_path: self.auth_path,
            periods,
            total_energy: self.split.total,
            direction: self.split.direction,
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
            // A tariff that was not in force when the session started did not
            // price it, whatever its numbers say. `[AFIR Art. 5(4)]`: the
            // price has to be known to the driver before they start, so the
            // version in force at that instant is the one that governs.
            if !tariff.covers(cdr.started_at) {
                return Err(CdrError::TariffNotInForce {
                    tariff_id: tariff.id.to_string(),
                    at: cdr.started_at,
                    valid_from: tariff.valid_from,
                    valid_until: tariff.valid_until,
                });
            }

            // The check the whole Eichrecht chain was built to make possible.
            // `[OCMF Tab. 19]` states how far the station's clock can be
            // trusted, and `[OCMF Tab. 7, EF]` flags a time value as unusable
            // separately from an energy one. A tariff that charges per minute —
            // the occupancy fee `[AFIR Art. 5(4)]` permits at 50 kW and above —
            // is billing a duration, and billing a duration off a clock the
            // signed record does not vouch for is billing a number nobody can
            // defend. The energy is unaffected, so the fix is a per-kWh tariff
            // rather than a blocked session.
            if let Some(evidence) = &cdr.evidence
                && !evidence.duration_billable
                && let Some(dimension) = time_dimension_of(tariff)
            {
                return Err(CdrError::DurationNotBillable { dimension });
            }

            // The same gate from the other end. A clock that cannot be *placed*
            // `[OCMF Tab. 19]` and a span that cannot be *resolved*
            // `[REA 6-A §3.1]` both leave a duration nobody can defend — and a
            // thirty-second session billed per minute is the second one.
            if let Some(dimension) = time_dimension_of(tariff)
                && !self.clock.permits(cdr.duration())
            {
                return Err(CdrError::DurationBelowClockResolution {
                    dimension,
                    measured: cdr.duration(),
                    shortest: self.clock.shortest_billable_span(),
                });
            }

            let chargeable = cdr.chargeable()?;
            cdr.cost = Some(Cost {
                tariff_id: tariff.id.clone(),
                tariff_fingerprint: tariff.fingerprint(),
                rated: emob_tariff::rate(tariff, &chargeable),
            });
        }

        Ok(cdr)
    }
}

/// The first time dimension a tariff prices anywhere, if it prices one.
fn time_dimension_of(tariff: &Tariff) -> Option<Dimension> {
    tariff
        .dimensions()
        .into_iter()
        .find(|d| matches!(d, Dimension::Time | Dimension::ParkingTime))
}

/// The quarter hours between two instants, as periods that moved no energy.
///
/// Split at the settlement boundaries like everything else, so an occupancy
/// interval that crosses 11:15 is two periods and the market side sees the same
/// grid the energy did.
fn occupancy_periods(from: time::OffsetDateTime, to: time::OffsetDateTime) -> Vec<ChargingPeriod> {
    let mut periods = Vec::new();
    if to <= from {
        return periods;
    }
    let mut slot = QuarterHour::containing(from);
    while slot.start() < to {
        let (start, end) = (slot.start().max(from), slot.end().min(to));
        // A slot the gap only touches at its edge contributes nothing.
        if end > start {
            periods.push(ChargingPeriod {
                quarter_hour: slot,
                start,
                end,
                energy: Energy::ZERO,
                // Time the meter said nothing about is time the vehicle was
                // connected and not charging — which is exactly what an
                // occupancy fee prices `[AFIR Art. 5(4)]`.
                charging: false,
                // The zero is assumed from the absence of readings, not
                // measured.
                provenance: Provenance::Interpolated,
            });
        }
        slot = slot.next();
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

    fn evidence(strength: IdentificationStrength) -> EvidenceRef {
        EvidenceRef {
            encoding_method: "OCMF".into(),
            payload_digests: vec![[1u8; 32], [2u8; 32]],
            identification_strength: strength,
            energy_billable: true,
            duration_billable: true,
            direction: Some(Direction::Import),
        }
    }

    fn tariff() -> Tariff {
        Tariff::simple(
            "ad-hoc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
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
        // The distinction that used to be guessed from `energy == 0`. A car at
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
            cdr.periods[1].charging,
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
        s.transition_to(emob_session::SessionState::Suspended, at(15))
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

        assert!(!cdr.periods[1].charging);
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
    fn a_session_wider_than_its_readings_claims_no_measurement_it_does_not_have() {
        // The station authorises at 10:00 and sends its first meter value at
        // 10:20. Clamping the settlement slot to the session window would make
        // the first period claim 10:15–10:30 for energy that was only measured
        // from 10:20, and would leave the twenty minutes before it unbilled as
        // occupancy. The slot carries its own window instead.
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
        assert!(cdr.periods.iter().take(2).all(|p| !p.charging));
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
        s.transition_to(emob_session::SessionState::Suspended, at(15))
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
                    // …and yet ten kilowatt-hours crossed the meter between
                    // 10:15 and 10:30.
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
            matches!(err, CdrError::EnergyWhileSuspended { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("records as suspended"));
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
            vec![
                PriceComponent::new(Dimension::Energy, dec("0.49")),
                PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
            ],
        );

        let err = CdrBuilder::from_session(&ended_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .evidence(unsynchronised.clone())
            .rated_with(&occupancy)
            .build()
            .unwrap_err();
        assert!(matches!(err, CdrError::DurationNotBillable { .. }), "{err}");
        assert!(err.to_string().contains("price this session per kWh"));

        // …and the energy is genuinely unaffected: the same session on a
        // per-kWh tariff builds and prices.
        let energy_only = Tariff::simple(
            "ad-hoc-dc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
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
            vec![PriceComponent::new(Dimension::Time, dec("6.00"))],
        );

        let err = CdrBuilder::from_session(&brief_session(), Direction::Import)
            .unwrap()
            .key(party(), "cdr-1".parse().unwrap())
            .rated_with(&by_the_minute)
            .build()
            .unwrap_err();

        assert!(
            matches!(err, CdrError::DurationBelowClockResolution { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("price this session per kWh"));

        // …and the energy is genuinely unaffected.
        let per_kwh = Tariff::simple(
            "ad-hoc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
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
