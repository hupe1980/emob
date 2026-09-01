//! Turning `ocpp-kit`'s version-neutral view into this crate's vocabulary.
//!
//! # One funnel, not three
//!
//! [`ocpp_kit::csms::events`] reduces OCPP 1.6, 2.0.1 and 2.1 to one
//! [`DomainEvent`], and it now carries everything a billable session needs: the
//! signed meter values verbatim, the reading context each one arrived under,
//! whether energy was flowing, and which EVSE. So this module is a `match` —
//! about eighty lines where it used to be three version-specific extractions of
//! a field the funnel dropped.
//!
//! Everything it used to do lives upstream now:
//!
//! | Was here | Is now |
//! |---|---|
//! | OCPP 1.6's `SignedMeterValueType` serialised into a `SampledValue.value` string `[OCA SMV §3.2.1]` | `v1_6::SampledValue::signed_meter_value` |
//! | `signedMeterData` as Base64 or plain | `metering::SignedMeterValue::decoded_str` |
//! | The `publicKey` envelope, in both shapes the field is sent in `[OCA SMV §3.2.2]` | `metering::decode_public_key` |
//! | Reaching past the funnel for the signed values | `DomainEvent`'s own `signed` field |
//!
//! That is protocol knowledge and it belongs in the protocol crate — where it
//! is also better than what was here: the envelope decoder splits on the first
//! two colons, and a key printed by `openssl` is colon-separated, so the
//! `base16` case the note exists for is exactly the one this crate used to get
//! wrong.
//!
//! # What is still a judgement
//!
//! Two mappings stay, because they are about **what emob does with a fact**
//! rather than about what the wire said:
//!
//! - a reading context this build does not know becomes `Sample.Periodic`, so
//!   it can never promote an assumption into a measurement;
//! - a stop reason it does not know becomes [`EndReason::Other`], because how a
//!   session ended shapes a dispute and never the arithmetic.

use emob_session::{EndReason, ReadingContext};
use ocpp_kit::csms::events::DomainEvent;
use ocpp_kit::types::DateTime;

use crate::transaction::{SignedReading, TransactionEvent};

/// An OCPP timestamp as an instant.
///
/// `ocpp-kit` carries time as a `jiff::Timestamp`, which is UTC by
/// construction; `time::OffsetDateTime` is what the rest of this workspace
/// speaks. Nanoseconds survive, and the offset is UTC because that is what the
/// source actually knows — a station's local offset is not on the wire.
#[must_use]
pub fn instant(at: DateTime) -> time::OffsetDateTime {
    let timestamp = at.timestamp();
    let nanos = i64::from(timestamp.subsec_nanosecond());
    time::OffsetDateTime::from_unix_timestamp(timestamp.as_second())
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        + time::Duration::nanoseconds(nanos)
}

/// A `ReadingContext` from its wire spelling.
///
/// One function for all three generations: OCPP spells these identically in
/// 1.6, 2.0.1 and 2.1, which is why the funnel hands them over as text.
///
/// An unrecognised context becomes `Sample.Periodic` — the schema's own default
/// and the **conservative** answer. Only `Sample.Clock` makes a settlement slot
/// measured rather than interpolated, so a context this build does not know can
/// never promote an assumption into a measurement.
#[must_use]
pub fn reading_context(wire: Option<&str>) -> ReadingContext {
    match wire.unwrap_or("Sample.Periodic") {
        "Transaction.Begin" => ReadingContext::TransactionBegin,
        "Transaction.End" => ReadingContext::TransactionEnd,
        "Sample.Clock" => ReadingContext::SampleClock,
        "Interruption.Begin" => ReadingContext::InterruptionBegin,
        "Interruption.End" => ReadingContext::InterruptionEnd,
        "Trigger" => ReadingContext::Trigger,
        "Other" => ReadingContext::Other,
        _ => ReadingContext::SamplePeriodic,
    }
}

/// Why a transaction stopped, from its wire spelling.
///
/// A reason this build does not know becomes [`EndReason::Other`] rather than a
/// failure: how a session ended shapes a dispute and never the arithmetic, so
/// an unrecognised one is worth recording and never worth refusing a CDR over.
#[must_use]
pub fn end_reason(wire: Option<&str>) -> EndReason {
    match wire.unwrap_or("Other") {
        "Local" | "StoppedByEV" => EndReason::Local,
        "Remote" => EndReason::Remote,
        "EVDisconnected" | "EVDisconnect" => EndReason::EvDisconnected,
        "DeAuthorized" => EndReason::DeAuthorized,
        "PowerLoss" => EndReason::PowerLoss,
        "EmergencyStop" | "HardReset" | "SoftReset" | "Reboot" => EndReason::Fault,
        _ => EndReason::Other,
    }
}

/// Whether a `chargingState` means energy is flowing.
///
/// Only `Charging` does. `EVConnected`, `SuspendedEV`, `SuspendedEVSE` and
/// `Idle` are all a vehicle plugged in and not charging — precisely what an
/// occupancy fee prices `[AFIR Art. 5(4)]`, and precisely what a meter reading
/// cannot tell you.
///
/// A station that sends no `chargingState` is reporting a transaction in
/// progress, so the answer is `true`: the alternative would price a whole
/// session as occupancy on a missing optional field. OCPP 1.6 never sends one —
/// it says the same thing through `StatusNotification` — so a 1.6 fleet needs
/// that event to reach the same conclusion.
#[must_use]
pub fn charging_from(wire: Option<&str>) -> bool {
    !matches!(
        wire,
        Some("EVConnected" | "SuspendedEV" | "SuspendedEVSE" | "Idle")
    )
}

/// The transaction event a [`DomainEvent`] describes, if it describes one.
///
/// `None` for everything that is not about a transaction — boot, heartbeat,
/// status, an authorization request. Those are the CSMS's business and not this
/// crate's.
///
/// The protocol's own numeric registers — `meter_start`, `meter`, `meter_stop`
/// — are **not read**, and that is the whole seam: they are telemetry, and in
/// the OCA's own example message `meterStop` reports the meter's lifetime
/// register while the signed data set reports the session `[OCA SMV §5.2]`.
#[must_use]
pub fn event_from(event: &DomainEvent) -> Option<TransactionEvent> {
    match event {
        DomainEvent::TransactionStarted {
            signed, timestamp, ..
        } => Some(TransactionEvent::started(
            instant(*timestamp),
            readings(signed),
        )),
        DomainEvent::TransactionUpdated {
            signed,
            charging_state,
            timestamp,
            ..
        } => Some(TransactionEvent {
            charging: charging_from(charging_state.as_deref()),
            ..TransactionEvent::updated(instant(*timestamp), readings(signed))
        }),
        DomainEvent::TransactionEnded {
            signed,
            stopped_reason,
            timestamp,
            ..
        } => Some(TransactionEvent::ended(
            instant(*timestamp),
            readings(signed),
            end_reason(stopped_reason.as_deref()),
        )),
        // `MeterValues` outside a transaction event is 1.6's mid-session
        // reporting. It belongs to a transaction when it names one, and the
        // caller routes it by that id.
        DomainEvent::MeterValues {
            signed,
            timestamp,
            transaction_id: Some(_),
            ..
        } => timestamp.map(|at| TransactionEvent::updated(instant(at), readings(signed))),
        _ => None,
    }
}

/// The funnel's readings, in this crate's spelling.
fn readings(signed: &[ocpp_kit::csms::events::SignedReading]) -> Vec<SignedReading> {
    signed.iter().cloned().map(Into::into).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::OCA_1_6_SAMPLED_VALUE;
    use ocpp_kit::csms::events::observe_v16;
    use ocpp_kit::v1_6;

    /// The OCA's own `StopTransaction.req`, through the funnel.
    fn oca_stop() -> v1_6::CsRequest {
        v1_6::CsRequest::StopTransaction(
            v1_6::StopTransactionRequest::new(
                108_814,
                DateTime::parse("2023-05-19T13:55:48Z").unwrap(),
                96,
            )
            .with_reason(v1_6::Reason::Local)
            .with_transaction_data(vec![v1_6::MeterValue::new(
                DateTime::parse("2023-05-19T13:55:48Z").unwrap(),
                vec![
                    v1_6::SampledValue::new(OCA_1_6_SAMPLED_VALUE.to_owned())
                        .with_format(v1_6::ValueFormat::SignedData)
                        .with_context(v1_6::ReadingContext::TransactionEnd),
                ],
            )]),
        )
    }

    #[test]
    fn the_signed_value_reaches_this_crate_through_the_funnel() {
        // What used to need three version-specific extractions: `DomainEvent`
        // carries the signed values now, so this is a `match`.
        let observed = observe_v16(&oca_stop());
        let event = event_from(&observed.event).expect("a transaction event");

        assert_eq!(event.kind, crate::EventKind::Ended);
        assert_eq!(event.stopped_because, Some(EndReason::Local));
        assert_eq!(event.signed.len(), 1);
        assert_eq!(
            event.signed[0].context.as_deref(),
            Some("Transaction.End"),
            "the protocol is the only thing that knows why a reading was taken"
        );

        // …and the record inside is the session's, not the protocol's register.
        let record = crate::record_of(&event.signed[0].value).unwrap();
        assert_eq!(
            record.payload.readings[1].value.unwrap().to_string(),
            "0.636"
        );
    }

    #[test]
    fn messages_that_are_not_about_a_transaction_yield_nothing() {
        let heartbeat = v1_6::CsRequest::Heartbeat(v1_6::HeartbeatRequest::new());
        assert!(event_from(&observe_v16(&heartbeat).event).is_none());
    }

    #[test]
    fn an_unknown_reading_context_never_promotes_an_assumption() {
        // `Sample.Clock` is the one context that makes a settlement slot
        // measured. A context this build does not know must not be it.
        assert_eq!(
            reading_context(Some("Sample.Clock")),
            ReadingContext::SampleClock
        );
        assert_eq!(
            reading_context(Some("Something.New")),
            ReadingContext::SamplePeriodic
        );
        assert_eq!(reading_context(None), ReadingContext::SamplePeriodic);
    }

    #[test]
    fn every_reading_context_survives_the_round_trip() {
        // `ReadingContext::as_str` writes the wire spelling and this reads it.
        // They are in different crates, so nothing but this test keeps them
        // each other's inverse — and a drift would silently demote a
        // `Sample.Clock` to an assumption.
        for context in [
            ReadingContext::TransactionBegin,
            ReadingContext::TransactionEnd,
            ReadingContext::SampleClock,
            ReadingContext::SamplePeriodic,
            ReadingContext::InterruptionBegin,
            ReadingContext::InterruptionEnd,
            ReadingContext::Trigger,
            ReadingContext::Other,
        ] {
            assert_eq!(reading_context(Some(context.as_str())), context);
        }
    }

    #[test]
    fn an_unknown_stop_reason_is_recorded_rather_than_refused() {
        assert_eq!(end_reason(Some("Local")), EndReason::Local);
        assert_eq!(
            end_reason(Some("EVDisconnected")),
            EndReason::EvDisconnected
        );
        assert_eq!(end_reason(Some("SomethingNew")), EndReason::Other);
        assert_eq!(end_reason(None), EndReason::Other);
    }

    #[test]
    fn a_charging_state_says_what_a_meter_cannot() {
        assert!(charging_from(Some("Charging")));
        assert!(
            charging_from(None),
            "a missing optional field is not occupancy"
        );
        for parked in ["EVConnected", "SuspendedEV", "SuspendedEVSE", "Idle"] {
            assert!(!charging_from(Some(parked)), "{parked}");
        }
    }

    #[test]
    fn an_ocpp_timestamp_becomes_the_same_instant() {
        let at = DateTime::parse("2023-05-19T13:55:48Z").unwrap();
        assert_eq!(
            instant(at),
            time::macros::datetime!(2023-05-19 13:55:48 UTC)
        );
    }
}
