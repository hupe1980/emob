//! The charge detail record: what two companies settle against.
//!
//! # Why a CDR is not just a session with a total on it
//!
//! A session is what happened. A CDR is a **claim** about what happened, sent
//! to somebody who was not there and who will pay against it. Three things
//! follow, and they are what this module enforces:
//!
//! 1. **It carries its own arithmetic.** The charging periods sum to the total,
//!    exactly, checked at construction — because the recipient will check, and
//!    finding out then costs a dispute.
//! 2. **It names its evidence.** Every CDR built here references the signed
//!    records it rests on by content digest, so "which meter values is this
//!    €14.46 made of" is answerable years later `[MessEG §33]`.
//! 3. **It is immutable and identified.** A CDR that can be edited in place is
//!    a CDR whose recipient and sender can hold different versions of the same
//!    id — which is the most common way roaming settlement goes wrong.

use emob_core::{CdrId, Direction, Energy, EvseId, PartyId, SessionId};
use emob_session::{AuthPath, Provenance, Session, SessionSplit};

use crate::error::CdrError;

/// One period of a CDR: a slice of the session with an energy attached.
///
/// Aligned to quarter hours when the CDR was built from a
/// [`SessionSplit`], because that is what the
/// German pass-through model settles in `[A6 §IV.1]`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ChargingPeriod {
    /// When the period begins.
    pub start: time::OffsetDateTime,
    /// The energy moved inside it.
    pub energy: Energy,
    /// How the number was arrived at — measured, or interpolated between two
    /// readings. Travels to the recipient, because a settlement dispute turns
    /// on it.
    pub provenance: Provenance,
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
    pub identification_strength: emob_session::IdentificationStrength,
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
    pub started_at: time::OffsetDateTime,
    /// When it ended.
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
}

/// Builds a CDR from a session, refusing to produce one that does not add up.
///
/// ```no_run
/// use emob_cdr::CdrBuilder;
/// # let session: emob_session::Session = unimplemented!();
/// # let party: emob_core::PartyId = unimplemented!();
/// # let evidence_ref: emob_cdr::EvidenceRef = unimplemented!();
///
/// let cdr = CdrBuilder::from_session(&session, emob_core::Direction::Import)?
///     .key(party, "cdr-1".parse()?)
///     .evidence(evidence_ref)
///     .build()?;
///
/// assert!(cdr.conserves());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct CdrBuilder {
    key: Option<CdrKey>,
    session_id: SessionId,
    evse_id: EvseId,
    started_at: time::OffsetDateTime,
    ended_at: time::OffsetDateTime,
    auth_path: AuthPath,
    split: SessionSplit,
    evidence: Option<EvidenceRef>,
    supersedes: Option<CdrKey>,
}

impl CdrBuilder {
    /// Start from a finished session.
    ///
    /// # Errors
    ///
    /// [`CdrError::SessionNotEnded`] when the session is still running — a CDR
    /// for a session in progress is a claim about a number that is still
    /// changing.
    /// [`CdrError::Session`] when the session cannot be split.
    pub fn from_session(session: &Session, direction: Direction) -> Result<Self, CdrError> {
        let ended_at = session.ended_at.ok_or(CdrError::SessionNotEnded)?;
        let split = session.split(direction)?;

        Ok(Self {
            key: None,
            session_id: session.id.clone(),
            evse_id: session.evse_id.clone(),
            started_at: session.started_at,
            ended_at,
            auth_path: session.authorization.path,
            split,
            evidence: None,
            supersedes: None,
        })
    }

    /// Give the CDR its key.
    #[must_use]
    pub fn key(mut self, party: PartyId, id: CdrId) -> Self {
        self.key = Some(CdrKey { party, id });
        self
    }

    /// Attach the signed evidence this CDR rests on.
    #[must_use]
    pub fn evidence(mut self, evidence: EvidenceRef) -> Self {
        self.evidence = Some(evidence);
        self
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
    /// [`CdrError::NoKey`] when no key was given, or
    /// [`CdrError::AuthStrengthMismatch`] when the session claims a stronger
    /// authorisation than its own signed record supports.
    pub fn build(self) -> Result<Cdr, CdrError> {
        let key = self.key.ok_or(CdrError::NoKey)?;

        // The cross-check nobody runs. A session that *claims* Plug & Charge
        // and whose signed record reports a bare RFID UID is telling two
        // stories about one event, and the weaker one is the one with a
        // signature behind it. Billing the stronger claim — a PnC tariff, a
        // contract that was never presented — is the kind of error that only
        // surfaces when a driver disputes it.
        if let Some(evidence) = &self.evidence {
            let ceiling = self.auth_path.strongest_plausible_level();
            if evidence.identification_strength > ceiling {
                return Err(CdrError::AuthStrengthMismatch {
                    claimed: self.auth_path,
                    ceiling,
                    signed: evidence.identification_strength,
                });
            }
        }

        let periods: Vec<ChargingPeriod> = self
            .split
            .slots
            .iter()
            .map(|slot| ChargingPeriod {
                start: slot.quarter_hour.start(),
                energy: slot.energy,
                provenance: slot.provenance,
            })
            .collect();

        let cdr = Cdr {
            key,
            session_id: self.session_id,
            evse_id: self.evse_id,
            started_at: self.started_at,
            ended_at: self.ended_at,
            auth_path: self.auth_path,
            periods,
            total_energy: self.split.total,
            direction: self.split.direction,
            evidence: self.evidence,
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

        Ok(cdr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_session::{
        Authorization, EndReason, IdentificationStrength, MeterReading, MeterSeries,
        ReadingContext, Session,
    };
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use time::macros::datetime;

    fn kwh(s: &str) -> Energy {
        Energy::from_kwh(Decimal::from_str(s).unwrap()).unwrap()
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
        s.transition_to(emob_session::SessionState::Charging)
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
        }
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
        s.transition_to(emob_session::SessionState::Charging)
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
}
