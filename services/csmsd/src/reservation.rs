//! A point held for one card (spec 025): `ReserveNow` and `CancelReservation`
//! on each OCPP version, and the station's answer as the one word it said.
//!
//! The station has one connector and one EVSE as emob provisions it, so a
//! reservation names connector 1 (1.6) or EVSE 1 (2.x). The UID travels to
//! the station — it must, to recognise the card — and nowhere else (SR-176).

use ocpp_kit::transport::CsmsHandle;
use ocpp_kit::types::Identity;
use ocpp_kit::{v1_6, v2_0_1, v2_1};
use serde_json::json;

use crate::display::{TIMEOUT, failure_name, wire};
use emob_service::estate::Ocpp;

/// The shortest and longest a point may be held, in minutes (SR-177).
pub const MINUTES: core::ops::RangeInclusive<u32> = 5..=1440;

/// The card as a person recognises it on a screen: `…F6`.
#[must_use]
pub fn tail(uid: &str) -> String {
    let chars: Vec<char> = uid.trim().chars().collect();
    let tail: String = chars[chars.len().saturating_sub(2)..].iter().collect();
    format!("…{}", tail.to_uppercase())
}

/// `14:30` — `until` on the wall clock of the zone named `zone`.
#[must_use]
pub fn wall_clock(zone: &str, until: time::OffsetDateTime) -> String {
    let zone = emob_core::TimeZone::new(zone).unwrap_or_else(|_| emob_core::TimeZone::utc());
    let local = zone.local(until);
    format!("{:02}:{:02}", local.time.hour(), local.time.minute())
}

fn rfc3339(at: time::OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn typed<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// Send `ReserveNow`; the station's status (`Accepted`, `Occupied`, …), or
/// why nothing was answered.
///
/// # Errors
///
/// No answer, a `CALLERROR`, or a request the kit would not build.
pub async fn reserve(
    handle: &CsmsHandle,
    target: &Identity,
    ocpp: Ocpp,
    id: i32,
    uid: &str,
    until: time::OffsetDateTime,
) -> Result<String, String> {
    let expiry = rfc3339(until);
    let token = json!({ "idToken": uid, "type": "ISO14443" });
    let answer = match ocpp {
        Ocpp::V1_6 => {
            let request: v1_6::ReserveNowRequest = typed(json!({
                "connectorId": 1, "expiryDate": expiry, "idTag": uid, "reservationId": id,
            }))?;
            tokio::time::timeout(TIMEOUT, handle.call(target, request))
                .await
                .map(|answer| answer.map(|answer| wire(&answer.status)))
        }
        Ocpp::V2_0_1 => {
            let request: v2_0_1::ReserveNowRequest = typed(json!({
                "id": id, "expiryDateTime": expiry, "idToken": token, "evseId": 1,
            }))?;
            tokio::time::timeout(TIMEOUT, handle.call(target, request))
                .await
                .map(|answer| answer.map(|answer| wire(&answer.status)))
        }
        Ocpp::V2_1 => {
            let request: v2_1::ReserveNowRequest = typed(json!({
                "id": id, "expiryDateTime": expiry, "idToken": token, "evseId": 1,
            }))?;
            tokio::time::timeout(TIMEOUT, handle.call(target, request))
                .await
                .map(|answer| answer.map(|answer| wire(&answer.status)))
        }
    };
    outcome(answer)
}

/// Send `CancelReservation`; the station's status, or why nothing was
/// answered.
///
/// # Errors
///
/// No answer, a `CALLERROR`, or a request the kit would not build.
pub async fn cancel(
    handle: &CsmsHandle,
    target: &Identity,
    ocpp: Ocpp,
    id: i32,
) -> Result<String, String> {
    let answer = match ocpp {
        Ocpp::V1_6 => tokio::time::timeout(
            TIMEOUT,
            handle.call(target, v1_6::CancelReservationRequest::new(id)),
        )
        .await
        .map(|answer| answer.map(|answer| wire(&answer.status))),
        Ocpp::V2_0_1 => tokio::time::timeout(
            TIMEOUT,
            handle.call(target, v2_0_1::CancelReservationRequest::new(id)),
        )
        .await
        .map(|answer| answer.map(|answer| wire(&answer.status))),
        Ocpp::V2_1 => tokio::time::timeout(
            TIMEOUT,
            handle.call(target, v2_1::CancelReservationRequest::new(id)),
        )
        .await
        .map(|answer| answer.map(|answer| wire(&answer.status))),
    };
    outcome(answer)
}

fn outcome(
    answer: Result<Result<String, ocpp_kit::engine::CallFailure>, tokio::time::error::Elapsed>,
) -> Result<String, String> {
    match answer {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(failure)) => Err(failure_name(&failure).to_owned()),
        Err(_) => Err("no answer".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_is_shown_by_its_last_two_characters_only() {
        assert_eq!(tail("04a1b2c3d4e5f6"), "…F6");
        assert_eq!(tail("7"), "…7");
    }
}
