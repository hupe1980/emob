//! An OCPP transaction, assembled into the session this workspace bills.
//!
//! # What comes from where
//!
//! Two sources describe one charging process and neither is sufficient alone:
//!
//! | Fact | Source | Why not the other |
//! |---|---|---|
//! | The register, and its scale | the **signed record** | OCPP's numeric fields are telemetry, and float by the time any ledger holds them |
//! | When the transaction opened and closed | the **OCPP events** | OCMF's clock is often `I` — informative `[OCMF Tab. 19]` — and the CSMS knows when it authorised |
//! | Whether the vehicle was charging or merely connected | the **OCPP events** | the meter cannot tell a taper from an occupancy, and `[AFIR Art. 5(4)]` prices them differently |
//! | Whether a reading landed on a clock boundary | the **OCPP `ReadingContext`** | nothing in the record says why it was taken |
//!
//! So [`Transaction::assemble`] takes the shape of the session from the
//! protocol and every number from the signature, which is the seam rule stated
//! as a construction rather than as a policy.
//!
//! # A retry is not a reading
//!
//! OCPP transports retry. A `MeterValues.req` that does not get its
//! confirmation in time is sent again, and the same signed record arrives
//! twice — with the same pagination counter, because the meter only produced
//! one. A CSMS that appends both hands the chain a duplicate, and the chain
//! answers `PaginationBreak`: a transport retry reported as a missing record,
//! on a session that is perfectly intact.
//!
//! Records are therefore de-duplicated by the digest of the bytes their
//! signature covers. Two records that hash the same *are* one record — that is
//! what the digest means — and two that differ are both kept, because a station
//! that reused a counter for different content is exactly the fault the chain
//! is there to find.

use emob_core::{Direction, EvseId, SessionId};
use emob_eichrecht::ocmf::{self, OcmfRecord};
use emob_session::{
    Authorization, EndReason, MeterReading, MeterSeries, ReadingContext, Session, SessionState,
};

use ocpp_kit::metering::SignedMeterValue;

use crate::error::SeamError;

/// One signed record together with the OCPP reading context it arrived under.
///
/// The context is the protocol's, and only the protocol has it: a record says
/// what the meter measured, never why the station took the reading. It is what
/// makes a settlement slot *measured* rather than interpolated, so it has to
/// survive de-duplication and re-ordering beside the record it belongs to.
type Delivered = (OcmfRecord, Option<String>);

/// Why a station sent a transaction event.
///
/// OCPP 2.x names these three `[OCPP 2.0.1 Part 2, TransactionEventEnumType]`;
/// OCPP 1.6 spells them `StartTransaction`, `MeterValues` and
/// `StopTransaction`. One vocabulary, because the difference between the two
/// generations is a wire detail and this is not the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum EventKind {
    /// The transaction opened.
    Started,
    /// Something happened during it — a periodic sample, a clock-aligned one,
    /// a state change.
    Updated,
    /// The transaction closed.
    Ended,
}

/// One signed meter value, and the context the sample around it named.
///
/// The same shape as [`ocpp_kit::csms::events::SignedReading`], and it exists
/// only because that one does not implement `Serialize`/`Deserialize` while
/// [`SignedMeterValue`] does — and a `TransactionEvent` has to survive being
/// persisted for crash recovery or forwarded to another service. Raised
/// upstream; when it lands this becomes a re-export.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SignedReading {
    /// The signed record, exactly as it arrived.
    pub value: SignedMeterValue,
    /// The sample's `context` — `Transaction.Begin`, `Sample.Clock`, … — when
    /// it named one. The protocol is the only thing that knows *why* a reading
    /// was taken, and `Sample.Clock` is what makes a settlement slot measured.
    pub context: Option<String>,
}

impl SignedReading {
    /// A reading in a context.
    #[must_use]
    pub const fn new(value: SignedMeterValue, context: Option<String>) -> Self {
        Self { value, context }
    }
}

impl From<ocpp_kit::csms::events::SignedReading> for SignedReading {
    fn from(reading: ocpp_kit::csms::events::SignedReading) -> Self {
        Self::new(reading.value, reading.context)
    }
}

/// One event a CSMS received about a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TransactionEvent {
    /// Which kind.
    pub kind: EventKind,
    /// When the station says it happened.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub at: time::OffsetDateTime,
    /// The signed values it carried, if any.
    pub signed: Vec<SignedReading>,
    /// Whether the station reports energy flowing at this point.
    ///
    /// OCPP 2.x states it directly as `chargingState`; 1.6 says it through
    /// `SuspendedEV` / `SuspendedEVSE` status notifications. It is a fact about
    /// the *session*, and the meter cannot supply it — which is why
    /// `[AFIR Art. 5(4)]`'s occupancy fee needs the protocol and not only the
    /// register.
    pub charging: bool,
    /// Why the transaction stopped, on an [`EventKind::Ended`] event.
    pub stopped_because: Option<EndReason>,
}

impl TransactionEvent {
    /// An opening event.
    #[must_use]
    pub fn started(at: time::OffsetDateTime, signed: Vec<SignedReading>) -> Self {
        Self {
            kind: EventKind::Started,
            at,
            signed,
            charging: true,
            stopped_because: None,
        }
    }

    /// An event during the transaction.
    #[must_use]
    pub fn updated(at: time::OffsetDateTime, signed: Vec<SignedReading>) -> Self {
        Self {
            kind: EventKind::Updated,
            at,
            signed,
            charging: true,
            stopped_because: None,
        }
    }

    /// A closing event.
    #[must_use]
    pub fn ended(at: time::OffsetDateTime, signed: Vec<SignedReading>, reason: EndReason) -> Self {
        Self {
            kind: EventKind::Ended,
            at,
            signed,
            charging: false,
            stopped_because: Some(reason),
        }
    }

    /// The same event, with the station reporting the vehicle connected and not
    /// charging.
    #[must_use]
    pub const fn suspended(mut self) -> Self {
        self.charging = false;
        self
    }
}

/// An OCPP transaction, as a CSMS holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Transaction {
    /// The station's own transaction identifier.
    pub id: SessionId,
    /// Which point it happened at.
    pub evse_id: EvseId,
    /// How it was authorised, as the CSMS decided it.
    pub authorization: Authorization,
    /// The events, in whatever order they arrived.
    pub events: Vec<TransactionEvent>,
}

/// A transaction turned into the two artefacts the rest of the workspace reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Assembled {
    /// The session, with its meter series taken from the signed records.
    pub session: Session,
    /// Those records, de-duplicated and in the order the meter produced them.
    ///
    /// Hand these to [`emob_eichrecht::Evidence::assemble`] together with a
    /// **registry**. Nothing here has verified anything: this crate's job ends
    /// at getting the bytes out of the transport intact.
    pub records: Vec<OcmfRecord>,
    /// How many duplicate records the transport delivered.
    ///
    /// Zero on a quiet link. Non-zero is not a fault — a retry is how OCPP
    /// guarantees delivery — but a link that retries constantly is one an
    /// operator wants to know about, and the number is otherwise invisible
    /// once the duplicates are dropped.
    pub duplicates_dropped: usize,
}

impl Transaction {
    /// A transaction with no events yet.
    #[must_use]
    pub const fn new(id: SessionId, evse_id: EvseId, authorization: Authorization) -> Self {
        Self {
            id,
            evse_id,
            authorization,
            events: Vec::new(),
        }
    }

    /// Record an event.
    #[must_use]
    pub fn with(mut self, event: TransactionEvent) -> Self {
        self.events.push(event);
        self
    }

    /// Each record with the OCPP context it arrived under, de-duplicated.
    fn deduplicated_records(&self) -> Result<(Vec<Delivered>, usize), SeamError> {
        let mut seen: Vec<[u8; 32]> = Vec::new();
        let mut delivered: Vec<Delivered> = Vec::new();
        let mut duplicates = 0;

        for event in &self.events {
            for reading in &event.signed {
                let record = record_of(&reading.value)?;
                let digest = ocmf::payload_digest(&record);
                if seen.contains(&digest) {
                    duplicates += 1;
                    continue;
                }
                seen.push(digest);
                delivered.push((record, reading.context.clone()));
            }
        }

        // The meter's own sequence, not the transport's. `sort_by_key` is
        // stable, so two records a station gave the same counter keep their
        // arrival order and reach the chain as the duplicate-counter finding
        // they are.
        delivered.sort_by_key(|(record, _)| record.payload.pagination.number);
        Ok((delivered, duplicates))
    }

    /// Assemble the session this transaction describes.
    ///
    /// `direction` says which register's series to build — the caller knows
    /// whether it is reading the import or the export side, and
    /// [`emob_cdr::CdrBuilder`] cross-checks it against what the signed OBIS
    /// code says `[OCMF Tab. 25]` rather than taking anybody's word for it.
    ///
    /// [`emob_cdr::CdrBuilder`]: https://docs.rs/emob-cdr
    ///
    /// # Errors
    ///
    /// [`SeamError::NoEvents`] for an empty transaction,
    /// [`SeamError::NoSignedValues`] when nothing signed arrived — the seam
    /// rule, as an error — [`SeamError::StillRunning`] when no event closed it,
    /// and [`SeamError::Session`] when the readings do not form a session.
    pub fn assemble(&self, direction: Direction) -> Result<Assembled, SeamError> {
        if self.events.is_empty() {
            return Err(SeamError::NoEvents);
        }
        let (delivered, duplicates_dropped) = self.deduplicated_records()?;
        if delivered.is_empty() {
            return Err(SeamError::NoSignedValues {
                transaction_id: self.id.to_string(),
            });
        }

        let mut events: Vec<&TransactionEvent> = self.events.iter().collect();
        events.sort_by_key(|e| e.at);
        // `started_at` is the protocol's, not the meter's: a station whose clock
        // is only informative `[OCMF Tab. 19]` still knows when the CSMS
        // authorised it, and that is the instant an occupancy fee runs from.
        let started_at = events.first().map_or_else(
            || delivered[0].0.payload.readings[0].time.instant,
            |event| event.at,
        );
        let ending = events
            .iter()
            .rev()
            .find(|e| e.kind == EventKind::Ended)
            .ok_or_else(|| SeamError::StillRunning {
                transaction_id: self.id.to_string(),
            })?;

        let mut session = Session::open(
            self.id.clone(),
            self.evse_id.clone(),
            self.authorization.clone(),
            started_at,
        );

        // The state machine, from the protocol's own account of when energy was
        // flowing. Only transitions are recorded, because the history is read
        // as intervals and a repeated state is not an interval boundary.
        let mut state = SessionState::Pending;
        for event in &events {
            if event.kind == EventKind::Ended {
                break;
            }
            let next = if event.charging {
                SessionState::Charging
            } else {
                SessionState::Suspended
            };
            if next != state {
                session.transition_to(next, event.at)?;
                state = next;
            }
        }

        session.attach_series(series_from(&delivered, direction)?)?;
        session.end(
            ending.at,
            ending.stopped_because.unwrap_or(EndReason::Other),
        )?;

        Ok(Assembled {
            session,
            records: delivered.into_iter().map(|(record, _)| record).collect(),
            duplicates_dropped,
        })
    }
}

/// The OCMF record inside a signed meter value.
///
/// `ocpp-kit` gets the bytes out of the transport — Base64 or plain, whichever
/// the station sent — and `emob-eichrecht` reads them. Neither step verifies
/// anything: that is [`emob_eichrecht::Evidence::assemble`], against a registry.
///
/// # Errors
///
/// [`SeamError::UnknownEncodingMethod`] for a format this workspace does not
/// read — EDL and the rest are refused by name, because a fleet configured for
/// one needs a different verifier rather than sessions that mysteriously will
/// not bill — [`SeamError::UndecodableSignedData`] when the transport layer is
/// malformed, and [`SeamError::BadRecord`] when the record itself is.
pub fn record_of(value: &SignedMeterValue) -> Result<OcmfRecord, SeamError> {
    if let Some(method) = &value.encoding_method
        && !method.eq_ignore_ascii_case("OCMF")
    {
        return Err(SeamError::UnknownEncodingMethod {
            encoding_method: method.clone(),
        });
    }
    let text = value
        .decoded_str()
        .map_err(|source| SeamError::UndecodableSignedData {
            detail: source.to_string(),
        })?;
    ocmf::parse(&text).map_err(|source| SeamError::BadRecord {
        detail: source.to_string(),
    })
}

/// The meter series, taken from the records rather than from the protocol.
///
/// Every reading is marked [`MeterReading::signed`], because every one of them
/// came out of a data set a private key covered. That is the whole point of the
/// seam: there is no path through this function for a number that was not
/// signed.
fn series_from(
    delivered: &[(OcmfRecord, Option<String>)],
    direction: Direction,
) -> Result<MeterSeries, SeamError> {
    let mut readings = Vec::new();
    for (record, context) in delivered {
        // The protocol's context describes **the value the event carried**, so
        // it applies when the record holds one reading — the ordinary case, one
        // signed value per `MeterValues.req`. A record holding several is the
        // `MR` configuration `[OCMF §Best Practice]`, where one OCPP context
        // cannot describe them all and the record's own markers are the only
        // thing that can.
        //
        // Reading it matters: `Sample.Clock` is the one context that makes a
        // settlement slot **measured** rather than interpolated, and a seam that
        // dropped it would silently mark every quarter hour in the fleet as an
        // assumption.
        let single = record.payload.readings.len() == 1;
        for reading in &record.payload.readings {
            // A reading without a usable register value is an event marker —
            // `[OCMF Tab. 7]` lets `RV` be omitted "if only the occurrence of an
            // error condition of the meter is to be indicated" — and it is the
            // chain's business rather than the series'.
            let Some(energy) = reading.energy() else {
                continue;
            };
            let context = if single {
                crate::kit::reading_context(context.as_deref())
            } else {
                context_of(reading.transaction)
            };
            readings
                .push(MeterReading::new(reading.time.instant, energy, direction, context).signed());
        }
    }
    Ok(MeterSeries::new(direction, readings)?)
}

/// The reading context a record's own transaction marker implies.
///
/// Only for a record holding **several** readings — the `MR` configuration,
/// where one signed data set holds the whole transaction
/// `[OCMF §Best Practice]` and the event's single OCPP context cannot describe
/// them all. A marker in the middle of such a set says nothing about clock
/// alignment, so it is `Sample.Periodic`: the honest answer, and the one that
/// keeps a settlement slot marked interpolated rather than claiming a
/// measurement nobody made.
fn context_of(marker: Option<ocmf::TransactionMarker>) -> ReadingContext {
    match marker {
        Some(m) if m.begins_transaction() => ReadingContext::TransactionBegin,
        Some(m) if m.ends_transaction() => ReadingContext::TransactionEnd,
        _ => ReadingContext::SamplePeriodic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{OCA_1_6_SAMPLED_VALUE, OCA_OCMF};

    fn evse() -> EvseId {
        "DE*SIM*E00001".parse().unwrap()
    }

    fn at(minute: i64) -> time::OffsetDateTime {
        time::macros::datetime!(2023-05-19 15:52:39 +2) + time::Duration::minutes(minute)
    }

    fn oca_reading(context: Option<String>) -> SignedReading {
        SignedReading::new(
            SignedMeterValue::from_signed_data(OCA_1_6_SAMPLED_VALUE).unwrap(),
            context,
        )
    }

    fn oca_transaction() -> Transaction {
        Transaction::new("t-96".parse().unwrap(), evse(), Authorization::ad_hoc())
            .with(TransactionEvent::started(at(0), vec![]))
            .with(TransactionEvent::ended(
                at(2),
                vec![oca_reading(Some("Transaction.End".to_owned()))],
                EndReason::Local,
            ))
    }

    #[test]
    fn a_real_ocpp_message_becomes_a_session_whose_energy_is_the_signed_one() {
        // The OCA's own example message. Its `meterStop` is 108814 — the
        // *lifetime* register in watt-hours — and the transaction's signed
        // difference is 0.636 kWh. A CSMS billing the protocol's numbers would
        // bill a number nothing signed, off a register that is not the
        // session's.
        let assembled = oca_transaction().assemble(Direction::Import).unwrap();

        assert_eq!(assembled.records.len(), 1);
        assert_eq!(
            assembled
                .session
                .total(Direction::Import)
                .unwrap()
                .to_string(),
            "0.636 kWh",
        );
        assert!(
            assembled
                .session
                .series_for(Direction::Import)
                .unwrap()
                .fully_signed(),
            "there is no path through the seam for an unsigned number"
        );
        assert_eq!(assembled.session.ended_at, Some(at(2)));
    }

    #[test]
    fn a_transaction_with_no_signed_value_is_refused_by_the_seam_rule() {
        // The rule as an error rather than a policy. There is no repair here,
        // only a different question — whether this jurisdiction bills unsigned
        // values at all `[MessEG §33]`.
        let bare = Transaction::new("t-1".parse().unwrap(), evse(), Authorization::ad_hoc())
            .with(TransactionEvent::started(at(0), vec![]))
            .with(TransactionEvent::ended(at(2), vec![], EndReason::Local));

        let error = bare.assemble(Direction::Import).unwrap_err();
        assert!(matches!(error, SeamError::NoSignedValues { .. }));
        assert!(error.to_string().contains("telemetry"));
    }

    #[test]
    fn a_retransmitted_record_is_one_record() {
        // OCPP retries. The same signed record arriving twice carries the same
        // pagination counter, because the meter produced one — so appending
        // both hands the chain a `PaginationBreak` on a session that is
        // perfectly intact.
        let retried = Transaction::new("t-96".parse().unwrap(), evse(), Authorization::ad_hoc())
            .with(TransactionEvent::started(at(0), vec![]))
            .with(TransactionEvent::updated(
                at(1),
                vec![oca_reading(Some("Sample.Periodic".to_owned()))],
            ))
            .with(TransactionEvent::ended(
                at(2),
                vec![oca_reading(Some("Transaction.End".to_owned()))],
                EndReason::Local,
            ));

        let assembled = retried.assemble(Direction::Import).unwrap();
        assert_eq!(assembled.records.len(), 1);
        assert_eq!(assembled.duplicates_dropped, 1);

        // …and the chain agrees it is a whole session.
        let report = emob_eichrecht::chain::validate(&assembled.records);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| matches!(f, emob_eichrecht::ChainFinding::PaginationBreak { .. })),
            "a transport retry is not a missing record: {:?}",
            report.findings
        );
        assert_eq!(report.billable_energy.unwrap().to_string(), "0.636 kWh");
    }

    #[test]
    fn records_arrive_in_the_meters_order_rather_than_the_transports() {
        // OCPP promises delivery, not sequence. The pagination counter is the
        // meter's own statement about order, and it is what the chain checks
        // contiguity against.
        let a = SignedMeterValue::new(record_text(1, "B", "0.000"));
        let b = SignedMeterValue::new(record_text(2, "C", "0.400"));
        let c = SignedMeterValue::new(record_text(3, "E", "0.636"));

        let shuffled = Transaction::new("t-1".parse().unwrap(), evse(), Authorization::ad_hoc())
            .with(TransactionEvent::started(
                at(0),
                vec![SignedReading::new(c, Some("Transaction.End".to_owned()))],
            ))
            .with(TransactionEvent::updated(
                at(1),
                vec![SignedReading::new(a, Some("Transaction.Begin".to_owned()))],
            ))
            .with(TransactionEvent::ended(
                at(2),
                vec![SignedReading::new(b, Some("Sample.Clock".to_owned()))],
                EndReason::Local,
            ));

        let assembled = shuffled.assemble(Direction::Import).unwrap();
        let counters: Vec<u64> = assembled
            .records
            .iter()
            .map(|r| r.payload.pagination.number)
            .collect();
        assert_eq!(counters, vec![1, 2, 3]);
    }

    /// A record with a chosen counter, marker and register value. Unsigned —
    /// these tests are about the seam's bookkeeping, and the signature is
    /// exercised against real hardware elsewhere.
    fn record_text(pagination: u64, marker: &str, value: &str) -> String {
        format!(
            concat!(
                r#"OCMF|{{"PG":"T{}","MS":"1DZG0028225179","RD":[{{"TM":"2023-05-19T15:5{}:00,000+0200 S","#,
                r#""TX":"{}","RV":{},"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}}]}}|{{"SD":"00"}}"#,
            ),
            pagination, pagination, marker, value,
        )
    }

    #[test]
    fn the_session_shape_comes_from_the_protocol_and_the_numbers_from_the_signature() {
        // The two sources, kept apart. The meter cannot tell a taper from an
        // occupancy; the protocol can, and `[AFIR Art. 5(4)]` prices them
        // differently.
        let transaction = Transaction::new("t-1".parse().unwrap(), evse(), Authorization::ad_hoc())
            .with(TransactionEvent::started(
                at(0),
                vec![SignedReading::new(
                    SignedMeterValue::new(record_text(1, "B", "0.000")),
                    Some("Transaction.Begin".to_owned()),
                )],
            ))
            .with(TransactionEvent::updated(at(1), vec![]).suspended())
            .with(TransactionEvent::updated(at(2), vec![]))
            .with(TransactionEvent::ended(
                at(3),
                vec![SignedReading::new(
                    SignedMeterValue::new(record_text(3, "E", "0.636")),
                    Some("Transaction.End".to_owned()),
                )],
                EndReason::EvDisconnected,
            ));

        let assembled = transaction.assemble(Direction::Import).unwrap();
        let session = &assembled.session;

        assert_eq!(session.end_reason, Some(EndReason::EvDisconnected));
        assert!(session.suspended_throughout(at(1), at(2)));
        assert!(!session.suspended_throughout(at(2), at(3)));
        assert_eq!(
            session.total(Direction::Import).unwrap().to_string(),
            "0.636 kWh"
        );
    }

    #[test]
    fn a_transaction_the_station_has_not_closed_is_not_a_session() {
        let running = Transaction::new("t-1".parse().unwrap(), evse(), Authorization::ad_hoc())
            .with(TransactionEvent::started(
                at(0),
                vec![oca_reading(Some("Transaction.Begin".to_owned()))],
            ));
        assert!(matches!(
            running.assemble(Direction::Import),
            Err(SeamError::StillRunning { .. })
        ));
        assert!(matches!(
            Transaction::new("t-1".parse().unwrap(), evse(), Authorization::ad_hoc())
                .assemble(Direction::Import),
            Err(SeamError::NoEvents)
        ));
    }

    #[test]
    fn one_bad_record_fails_the_transaction_rather_than_vanishing() {
        // Skipping it would hide exactly what the pagination check exists to
        // find: a session missing a record it was sent.
        let broken = Transaction::new("t-1".parse().unwrap(), evse(), Authorization::ad_hoc())
            .with(TransactionEvent::started(
                at(0),
                vec![SignedReading::new(
                    SignedMeterValue::new("not a record"),
                    Some("Transaction.Begin".to_owned()),
                )],
            ))
            .with(TransactionEvent::ended(at(2), vec![], EndReason::Local));

        // Reported as `BadRecord` rather than `UndecodableSignedData`: text that
        // is neither Base64 nor an `OCMF|` record is handed through as-is by
        // `decoded()`, so the OCMF parser is what names it. Either way the
        // transaction fails rather than losing a record silently.
        assert!(matches!(
            broken.assemble(Direction::Import),
            Err(SeamError::BadRecord { .. })
        ));
    }

    #[test]
    fn a_whole_transaction_in_one_data_set_is_a_session_too() {
        // The OCA's example is the `MR` configuration: both readings in one
        // signed set, delivered on the closing event. There is no per-reading
        // protocol context to read, so nothing claims a clock-aligned
        // measurement that was never made.
        let assembled = oca_transaction().assemble(Direction::Import).unwrap();
        let series = assembled.session.series_for(Direction::Import).unwrap();

        assert_eq!(series.readings().len(), 2);
        assert_eq!(series.clock_aligned_count(), 0);
        assert_eq!(
            series.readings()[0].context,
            ReadingContext::TransactionBegin
        );
        assert_eq!(series.readings()[1].context, ReadingContext::TransactionEnd);
        assert!(assembled.records[0].as_str().contains(&OCA_OCMF[..40]));
    }
}
