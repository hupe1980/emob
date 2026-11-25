//! The receipt on the station's display (spec 011).
//!
//! When a session's receipt is filed, a station the estate opted in with
//! `receipt_display_minutes` is sent one `SetDisplayMessage`
//! `[OCPP 2.0.1 Part 2 §O]` reading `Beleg: <link>` until now plus those
//! minutes. A screen is public: whoever stands there next can read the link,
//! which is why the option is off by default, bounded to 1–15 minutes and the
//! receipt it opens shows no name, card or contract (SR-126..SR-128).
//!
//! The message is a courtesy, not a fact: one attempt on its own task, bounded
//! by a timeout, after the receipt and the verdict are journalled — never
//! retried, never journalled, and logged with the station and the outcome only,
//! never the link (SR-129..SR-131). OCPP 1.6 has no display message; a 1.6
//! station with the option is named once at start-up and skipped (SR-132).

use std::collections::BTreeMap;
use std::time::Duration;

use emob_service::estate::{EstateError, Ocpp, Station};
use ocpp_kit::transport::CsmsHandle;
use ocpp_kit::types::{DateTime, Identity};
use ocpp_kit::{v2_0_1, v2_1};
use sha2::{Digest as _, Sha256};

/// The longest a station may show the link, in minutes.
pub const MAX_MINUTES: u8 = 15;

/// How long one `SetDisplayMessage` may take before it is given up.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The 2.0.1 and 2.1 stations that show their receipt link, and for how long.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Display {
    stations: BTreeMap<String, (Ocpp, u8)>,
}

impl Display {
    /// The opted-in stations of the estate. A 1.6 station with the option is
    /// named in a log line and left out.
    ///
    /// # Errors
    ///
    /// [`EstateError::Invalid`] naming `stations[i].receipt_display_minutes`
    /// for a value outside 1–15.
    pub fn from_estate(stations: &[Station]) -> Result<Self, EstateError> {
        let mut display = Self::default();
        for (index, station) in stations.iter().enumerate() {
            let Some(minutes) = station.receipt_display_minutes else {
                continue;
            };
            if !(1..=MAX_MINUTES).contains(&minutes) {
                return Err(EstateError::Invalid {
                    key: format!("stations[{index}].receipt_display_minutes"),
                    reason: format!(
                        "{minutes} is not 1–{MAX_MINUTES}: the link stays on a public screen \
                         that long"
                    ),
                });
            }
            if station.ocpp == Ocpp::V1_6 {
                tracing::warn!(
                    station = %station.identity,
                    "{} speaks OCPP 1.6, which has no display message — \
                     receipt_display_minutes is skipped",
                    station.identity
                );
                continue;
            }
            display
                .stations
                .insert(station.identity.clone(), (station.ocpp, minutes));
        }
        Ok(display)
    }

    /// Whether `identity` shows its receipt link.
    #[must_use]
    pub fn shows(&self, identity: &str) -> bool {
        self.stations.contains_key(identity)
    }

    /// Send `link` to `identity` — the station the receipt is for, and no
    /// other (SR-129) — on its own task (SR-131). Nothing for a station that
    /// did not opt in (SR-128).
    pub fn send(&self, handle: &CsmsHandle, identity: &str, transaction: &str, link: String) {
        let Some(&(ocpp, minutes)) = self.stations.get(identity) else {
            return;
        };
        Self::send_as(handle, identity, ocpp, minutes, transaction, link);
    }

    /// The same, for a station as the registry holds it (spec 021): nothing
    /// for 1.6 or a station that did not opt in.
    pub fn send_registered(
        handle: &CsmsHandle,
        station: &emob_service::estate::Station,
        transaction: &str,
        link: String,
    ) {
        let Some(minutes) = station.receipt_display_minutes else {
            return;
        };
        if station.ocpp == Ocpp::V1_6 || !(1..=MAX_MINUTES).contains(&minutes) {
            return;
        }
        Self::send_as(
            handle,
            &station.identity,
            station.ocpp,
            minutes,
            transaction,
            link,
        );
    }

    fn send_as(
        handle: &CsmsHandle,
        identity: &str,
        ocpp: Ocpp,
        minutes: u8,
        transaction: &str,
        link: String,
    ) {
        let Ok(target) = Identity::new(identity) else {
            return;
        };
        let handle = handle.clone();
        let station = identity.to_owned();
        let id = message_id(identity, transaction);
        let ends = time::OffsetDateTime::now_utc() + time::Duration::minutes(i64::from(minutes));
        tokio::spawn(async move {
            let outcome = match ocpp {
                Ocpp::V2_0_1 => {
                    let request = message_v201(id, &link, ends);
                    tokio::time::timeout(TIMEOUT, handle.call(&target, request))
                        .await
                        .map(|answer| answer.map(|answer| wire(&answer.status)))
                }
                _ => {
                    let request = message_v21(id, &link, ends);
                    tokio::time::timeout(TIMEOUT, handle.call(&target, request))
                        .await
                        .map(|answer| answer.map(|answer| wire(&answer.status)))
                }
            };
            // The station and the outcome — never the link (SR-130).
            let status = match outcome {
                Ok(Ok(status)) => status,
                Ok(Err(failure)) => failure_name(&failure).to_owned(),
                Err(_) => "no answer".to_owned(),
            };
            tracing::info!(%station, %status, "display message");
        });
    }
}

/// `Beleg: <link>` — what the screen reads.
#[must_use]
pub fn content(link: &str) -> String {
    format!("Beleg: {link}")
}

/// A stable id per transaction, so a re-send replaces rather than stacks.
#[must_use]
pub fn message_id(identity: &str, transaction: &str) -> i32 {
    let digest = Sha256::new()
        .chain_update(identity.as_bytes())
        .chain_update([0])
        .chain_update(transaction.as_bytes())
        .finalize();
    i32::from_be_bytes([digest[0] & 0x7f, digest[1], digest[2], digest[3]])
}

pub(crate) fn until(ends: time::OffsetDateTime) -> Option<DateTime> {
    ends.format(&time::format_description::well_known::Rfc3339)
        .ok()
        .and_then(|text| DateTime::parse(&text).ok())
}

/// The 2.0.1 request: `InFront`, `UTF8`, German, ending at `ends` (SR-127).
#[must_use]
pub fn message_v201(
    id: i32,
    link: &str,
    ends: time::OffsetDateTime,
) -> v2_0_1::SetDisplayMessageRequest {
    let mut info = v2_0_1::MessageInfo::new(
        id,
        v2_0_1::MessagePriority::InFront,
        v2_0_1::MessageContent::new(v2_0_1::MessageFormat::UTF8, content(link)).with_language("de"),
    );
    info.end_date_time = until(ends);
    v2_0_1::SetDisplayMessageRequest::new(info)
}

/// The 2.1 request, the same message.
#[must_use]
pub fn message_v21(
    id: i32,
    link: &str,
    ends: time::OffsetDateTime,
) -> v2_1::SetDisplayMessageRequest {
    let mut info = v2_1::MessageInfo::new(
        id,
        v2_1::MessagePriority::InFront,
        v2_1::MessageContent::new(v2_1::MessageFormat::UTF8, content(link)).with_language("de"),
    );
    info.end_date_time = until(ends);
    v2_1::SetDisplayMessageRequest::new(info)
}

pub(crate) fn wire(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_default()
}

pub(crate) const fn failure_name(failure: &ocpp_kit::engine::CallFailure) -> &'static str {
    use ocpp_kit::engine::CallFailure;
    match failure {
        CallFailure::Rejected(_) => "CALLERROR",
        CallFailure::Timeout => "no answer",
        CallFailure::Disconnected => "not connected",
        _ => "not sent",
    }
}

fn suppress_attempt(
    previous: &crate::journal::StationTariff,
    version: &str,
    now: time::OffsetDateTime,
) -> bool {
    previous.version == version
        && (!previous.retry || now - previous.at < time::Duration::minutes(1))
}

/// Install the effective tariff on a connected, opted-in 2.1 station after boot.
pub(crate) async fn install_tariff<J: emob_service::journal::Journal>(
    csms: &crate::Csmsd<J>,
    handle: &CsmsHandle,
    identity: &Identity,
) {
    let selected = csms.with_registry(|registry, state| {
        let station = registry.get(identity.as_str())?;
        if !station.show_price || !station.publicly_accessible || station.ocpp != Ocpp::V2_1 {
            return None;
        }
        let known = state.stations.get(identity.as_str())?;
        Some((
            station.tariff.clone(),
            known.facility_version,
            known.boot_generation,
            known.tariff_boot?,
            known.station_tariff.clone(),
        ))
    });
    let Some((tariff_id, facility_version, boot_generation, boot, previous)) = selected else {
        return;
    };
    let Some(session) = handle.session(identity).await else {
        return;
    };
    if session
        .session::<crate::ChargePoint>()
        .is_none_or(|p| p.ocpp != ocpp_kit::Version::V2_1)
    {
        return;
    }
    let now = csms.now();
    let Some(tariff) = csms.tariff(&tariff_id).and_then(|h| h.in_force_at(now)) else {
        return;
    };
    let version = tariff.fingerprint().to_string();
    if previous
        .as_ref()
        .is_some_and(|delivery| suppress_attempt(delivery, &version, now))
    {
        return;
    }
    let (outcome, retry) = match emob_ocpp::tariff::to_ocpp(tariff, now) {
        Ok(wire) if wire.is_lossless() => {
            match tokio::time::timeout(
                TIMEOUT,
                session.call(v2_1::SetDefaultTariffRequest::new(1, wire.value)),
            )
            .await
            {
                Ok(Ok(answer)) => (self::wire(&answer.status), false),
                Ok(Err(ocpp_kit::engine::CallFailure::Rejected(error))) => {
                    // Omit the peer's free-text description, details and non-standard codes.
                    let code = match error.code {
                        ocpp_kit::rpc::ErrorCode::Other(_) => "Other",
                        ref code => code.as_wire(ocpp_kit::Version::V2_1),
                    };
                    (format!("CALLERROR {code}"), false)
                }
                Ok(Err(error)) => (failure_name(&error).to_owned(), true),
                Err(_) => ("no answer".to_owned(), true),
            }
        }
        Ok(crossing) => (
            crossing
                .notes()
                .iter()
                .map(|n| n.reason.as_str())
                .collect::<Vec<_>>()
                .join(" · "),
            false,
        ),
        Err(error) => (error.to_string(), false),
    };
    let delivery = crate::journal::StationTariff {
        version,
        facility_version,
        boot_generation,
        boot,
        at: csms.now(),
        outcome,
        retry,
    };
    if let Err(error) = csms.record(
        &emob_service::Actor::Timer,
        crate::journal::Event::StationTariff {
            station: identity.to_string(),
            delivery,
        },
    ) {
        tracing::error!(%error, "station tariff response could not be recorded");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stations() -> Vec<Station> {
        let estate: crate::estate::Estate =
            emob_service::estate::from_str(include_str!("../../../deploy/estate.example.toml"))
                .expect("the example estate");
        estate.stations
    }

    #[test]
    fn sr_209_a_late_station_receipt_cannot_replace_the_current_boot_or_registration() {
        use crate::journal::{Event, State, StationTariff, apply};
        let mut state = State::new(emob_core::TimeZone::utc());
        let station = stations().remove(0);
        let name = station.identity.clone();
        let at = time::macros::datetime!(2026-11-25 12:00 UTC);
        let register = Event::StationRegistered {
            station: Box::new(station),
            at,
            offers: vec![],
        };
        let boot = Event::Booted {
            station: name.clone(),
            at,
            reason: "PowerUp".into(),
            identity: Default::default(),
        };
        let receipt = |boot_generation, facility_version| Event::StationTariff {
            station: name.clone(),
            delivery: StationTariff {
                version: "e".repeat(64),
                facility_version,
                boot_generation,
                boot: at,
                at,
                outcome: "Accepted".to_owned(),
                retry: false,
            },
        };
        apply(&mut state, register.clone());
        apply(&mut state, boot.clone());
        apply(&mut state, receipt(1, 1));
        assert_eq!(
            state.stations[&name]
                .station_tariff
                .as_ref()
                .unwrap()
                .outcome,
            "Accepted"
        );
        // A controlled clock can give two distinct boots exactly the same timestamp.
        apply(&mut state, boot.clone());
        apply(&mut state, receipt(1, 1));
        assert!(state.stations[&name].station_tariff.is_none());
        apply(&mut state, receipt(2, 1));
        assert_eq!(
            state.stations[&name]
                .station_tariff
                .as_ref()
                .unwrap()
                .boot_generation,
            2
        );
        apply(
            &mut state,
            Event::StationRetired {
                identity: name.clone(),
                reason: "retired".into(),
                at,
            },
        );
        apply(&mut state, receipt(2, 1));
        assert!(state.stations[&name].station_tariff.is_none());
        apply(&mut state, register);
        apply(&mut state, boot);
        apply(&mut state, receipt(3, 1));
        assert!(state.stations[&name].station_tariff.is_none());
        apply(&mut state, receipt(3, 2));
        assert!(state.stations[&name].station_tariff.is_some());
    }

    #[test]
    fn sr_209_transport_failures_retry_after_a_minute_and_a_terminal_station_result_stops() {
        let now = time::macros::datetime!(2026-11-25 12:00 UTC);
        let version = "a".repeat(64);
        let mut attempt = crate::journal::StationTariff {
            version: version.clone(),
            facility_version: 1,
            boot_generation: 1,
            boot: now,
            at: now,
            outcome: "no answer".into(),
            retry: true,
        };
        assert!(suppress_attempt(
            &attempt,
            &version,
            now + time::Duration::seconds(59)
        ));
        assert!(!suppress_attempt(
            &attempt,
            &version,
            now + time::Duration::seconds(60)
        ));
        attempt.outcome = "CALLERROR NotSupported".into();
        attempt.retry = false;
        assert!(suppress_attempt(
            &attempt,
            &version,
            now + time::Duration::hours(1)
        ));
        assert!(!suppress_attempt(&attempt, &"b".repeat(64), now));
    }

    #[test]
    fn sr_128_a_station_without_the_option_shows_nothing() {
        let display = Display::from_estate(&stations()).unwrap();
        assert_eq!(display, Display::default());
    }

    #[test]
    fn sr_126_minutes_outside_one_to_fifteen_are_refused_by_key() {
        for minutes in [0, 16] {
            let mut stations = stations();
            stations[0].ocpp = Ocpp::V2_1;
            stations[0].receipt_display_minutes = Some(minutes);
            let error = Display::from_estate(&stations).expect_err("out of range");
            assert!(
                error
                    .to_string()
                    .contains("stations[0].receipt_display_minutes"),
                "{error}"
            );
        }
    }

    #[test]
    fn sr_132_a_one_six_station_is_skipped_and_a_two_x_one_shows() {
        let mut stations = stations();
        stations[0].ocpp = Ocpp::V1_6;
        stations[0].receipt_display_minutes = Some(5);
        assert!(!Display::from_estate(&stations).unwrap().shows("CP-1"));
        stations[0].ocpp = Ocpp::V2_0_1;
        assert!(Display::from_estate(&stations).unwrap().shows("CP-1"));
    }

    #[test]
    fn sr_127_the_message_reads_beleg_and_ends_after_the_minutes() {
        let now = time::macros::datetime!(2026-06-01 08:00 UTC);
        let ends = now + time::Duration::minutes(5);
        let link = "https://emob.example/r/AAAAAAAAAAAAAAAAAAAAAA";
        let json = serde_json::to_value(message_v21(7, link, ends)).unwrap();
        assert_eq!(
            json["message"]["message"]["content"],
            format!("Beleg: {link}")
        );
        assert_eq!(json["message"]["message"]["format"], "UTF8");
        assert_eq!(json["message"]["message"]["language"], "de");
        assert_eq!(json["message"]["priority"], "InFront");
        assert_eq!(json["message"]["endDateTime"], "2026-06-01T08:05:00Z");
        let json = serde_json::to_value(message_v201(7, link, ends)).unwrap();
        assert_eq!(json["message"]["endDateTime"], "2026-06-01T08:05:00Z");
    }

    #[test]
    fn the_message_id_is_stable_per_transaction_and_never_negative() {
        let a = message_id("CP-3", "tx-1");
        assert_eq!(a, message_id("CP-3", "tx-1"));
        assert_ne!(a, message_id("CP-3", "tx-2"));
        assert!(a >= 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sr_129_the_link_reaches_its_own_station_and_no_other() {
        use ocpp_kit::testkit::{MockCsms, MockStation};
        let csms = MockCsms::builder()
            .versions([ocpp_kit::Version::V2_1])
            .start()
            .await
            .unwrap();
        let shows = MockStation::connect(csms.url(), "CP-3").unwrap();
        let other = MockStation::connect(csms.url(), "CP-2").unwrap();
        for station in [&shows, &other] {
            station.boot(ocpp_kit::Version::V2_1).await.unwrap();
        }
        let mut stations = stations();
        stations[0].identity = "CP-3".to_owned();
        stations[0].ocpp = Ocpp::V2_1;
        stations[0].receipt_display_minutes = Some(5);
        let display = Display::from_estate(&stations).unwrap();
        let link = "http://127.0.0.1/r/BBBBBBBBBBBBBBBBBBBBBB";
        // SR-131: `send` hands the call to its own task and returns.
        display.send(csms.handle(), "CP-3", "tx-9", link.to_owned());
        display.send(csms.handle(), "CP-2", "tx-9", link.to_owned());
        for _ in 0..200 {
            if shows.saw("SetDisplayMessage") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let seen = shows.exchanges();
        let message = seen
            .iter()
            .find(|exchange| exchange.action == "SetDisplayMessage")
            .expect("CP-3 was sent its link");
        assert!(
            message.payload.contains(&content(link)),
            "{}",
            message.payload
        );
        assert!(!other.saw("SetDisplayMessage"), "CP-2 never opted in");
    }

    /// A station that takes every request and answers none.
    struct Silent;

    impl ocpp_kit::transport::Handler for Silent {
        fn on_request(
            &self,
            _ctx: ocpp_kit::transport::Ctx,
            _request: ocpp_kit::engine::IncomingRequest,
        ) -> ocpp_kit::transport::BoxFuture<
            '_,
            Result<Box<ocpp_kit::RawValue>, ocpp_kit::rpc::CallError>,
        > {
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sr_131_a_station_that_never_answers_holds_nobody_up() {
        use ocpp_kit::transport::{BasicAuthPassword, SecurityProfile, Station as Kit};
        let csms = ocpp_kit::testkit::MockCsms::builder()
            .versions([ocpp_kit::Version::V2_1])
            .start()
            .await
            .unwrap();
        let silent = Kit::builder()
            .identity("CP-3")
            .unwrap()
            .url(csms.url())
            .versions([ocpp_kit::Version::V2_1])
            .security_profile(SecurityProfile::BasicAuth)
            .password(BasicAuthPassword::utf8("a-silent-station-secret").unwrap())
            .ping_interval(None)
            .handler(Silent)
            .build()
            .unwrap()
            .spawn()
            .unwrap();
        let boot: ocpp_kit::v2_1::BootNotificationRequest = serde_json::from_value(
            serde_json::json!({"reason": "PowerUp", "chargingStation": {"model": "M", "vendorName": "V"}}),
        )
        .unwrap();
        silent.call(boot).await.unwrap();
        let mut stations = stations();
        stations[0].identity = "CP-3".to_owned();
        stations[0].ocpp = Ocpp::V2_1;
        stations[0].receipt_display_minutes = Some(1);
        let display = Display::from_estate(&stations).unwrap();
        let began = std::time::Instant::now();
        for transaction in ["tx-1", "tx-2", "tx-3"] {
            display.send(
                csms.handle(),
                "CP-3",
                transaction,
                "http://x/r/y".to_owned(),
            );
        }
        assert!(
            began.elapsed() < Duration::from_millis(50),
            "send waited for the station: {:?}",
            began.elapsed()
        );
        // The station's own requests are still answered while three display
        // calls hang.
        let heartbeat = tokio::time::timeout(
            Duration::from_secs(5),
            silent.call(ocpp_kit::v2_1::HeartbeatRequest::new()),
        )
        .await;
        assert!(matches!(heartbeat, Ok(Ok(_))), "{heartbeat:?}");
    }
}
