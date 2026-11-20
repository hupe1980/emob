//! What a station is, as the platform knows it (spec 012).
//!
//! Two sources, and they are not equally strong. What a station says about
//! itself in `BootNotification` — vendor, model, serial, firmware — is a claim
//! on an authenticated socket. What its meter signs — `MV`, `MM`, `MS`, `MF`,
//! `GV` in every OCMF record — is evidence, taken here only from a session that
//! billed, which is to say one whose signatures verified against the registry.
//! The page shows both, labelled as what they are.
//!
//! A change of either firmware is an intervention on the measuring instrument
//! whose record `[MessEG §31(2) Nr. 4]` makes the operator keep — "Nachweise
//! über … Eingriffe am Messgerät, einschließlich solcher durch elektronisch
//! vorgenommene Maßnahmen". The fold keeps the last ten changes per station;
//! the journal keeps every one.

use serde::{Deserialize, Serialize};

/// The longest identity string kept: OCPP's own limit for these fields
/// (`CiString50Type` / `string[0..50]`).
pub const MAX_LEN: usize = 50;

/// How many firmware changes and reports the read model keeps per station.
pub const KEPT: usize = 10;

/// What a station says it is, at boot.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StationIdentity {
    /// The manufacturer.
    pub vendor: String,
    /// The model.
    pub model: String,
    /// The serial number, when it sends one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    /// The firmware version, when it sends one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    /// The mobile network its SIM belongs to — the IMSI's MCC and MNC, never
    /// the IMSI (spec 022, SR-149).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
}

/// The mobile network of an IMSI: its country code and network code, and
/// nothing that identifies the SIM (SR-149). An IMSI is 6–15 digits (ITU-T
/// E.212); anything else names no network (SR-150). The network
/// code is three digits under the North American country codes (3xx) and two
/// elsewhere — where Europe's carriers are.
#[must_use]
pub fn network(imsi: Option<&String>) -> Option<String> {
    let imsi = imsi?.trim();
    if !(6..=15).contains(&imsi.len()) || !imsi.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let digits = if imsi.starts_with('3') { 6 } else { 5 };
    Some(imsi[..digits].to_owned())
}

/// What a meter signs about itself.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MeterIdentity {
    /// `MV`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// `MM`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `MS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    /// `MF`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    /// `GV` — the signing component's software version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_version: Option<String>,
}

impl MeterIdentity {
    /// From a record's payload. `None` when it names nothing.
    #[must_use]
    pub fn of(payload: &ocmf::Payload<'_>) -> Option<Self> {
        let identity = Self {
            vendor: payload.meter_vendor().map(clean),
            model: payload.meter_model().map(clean),
            serial: payload.meter_serial().map(clean),
            firmware: payload.meter_firmware().map(clean),
            gateway_version: payload.gateway_version().map(clean),
        };
        (identity != Self::default()).then_some(identity)
    }

    /// The firmware a change is recorded against: the meter's own, else the
    /// signing component's.
    #[must_use]
    pub fn firmware_label(&self) -> Option<&str> {
        self.firmware.as_deref().or(self.gateway_version.as_deref())
    }
}

/// A station-supplied string as it may be kept and shown: control characters
/// removed, trimmed, at most [`MAX_LEN`] characters (SR-137).
#[must_use]
pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_LEN)
        .collect()
}

fn clean_opt(text: Option<&String>) -> Option<String> {
    text.map(|t| clean(t)).filter(|t| !t.is_empty())
}

/// From a 1.6 `BootNotification` `[OCPP 1.6 §4.2]`. The charge point's serial,
/// else the charge box's.
#[must_use]
pub fn of_v1_6(boot: &ocpp_kit::v1_6::BootNotificationRequest) -> StationIdentity {
    StationIdentity {
        vendor: clean(&boot.charge_point_vendor),
        model: clean(&boot.charge_point_model),
        serial: clean_opt(boot.charge_point_serial_number.as_ref())
            .or_else(|| clean_opt(boot.charge_box_serial_number.as_ref())),
        firmware: clean_opt(boot.firmware_version.as_ref()),
        network: network(boot.imsi.as_ref()),
    }
}

/// From a 2.0.1 `BootNotification` `[OCPP 2.0.1 Part 2 §B01]`.
#[must_use]
pub fn of_v2_0_1(boot: &ocpp_kit::v2_0_1::BootNotificationRequest) -> StationIdentity {
    let station = &boot.charging_station;
    StationIdentity {
        vendor: clean(&station.vendor_name),
        model: clean(&station.model),
        serial: clean_opt(station.serial_number.as_ref()),
        firmware: clean_opt(station.firmware_version.as_ref()),
        network: network(station.modem.as_ref().and_then(|modem| modem.imsi.as_ref())),
    }
}

/// From a 2.1 `BootNotification`.
#[must_use]
pub fn of_v2_1(boot: &ocpp_kit::v2_1::BootNotificationRequest) -> StationIdentity {
    let station = &boot.charging_station;
    StationIdentity {
        vendor: clean(&station.vendor_name),
        model: clean(&station.model),
        serial: clean_opt(station.serial_number.as_ref()),
        firmware: clean_opt(station.firmware_version.as_ref()),
        network: network(station.modem.as_ref().and_then(|modem| modem.imsi.as_ref())),
    }
}

/// What a station reported about itself that settles nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Report {
    /// `FirmwareStatusNotification`: where an update stands.
    Firmware {
        /// `Downloaded`, `InstallationFailed`, `Installed`, …
        status: String,
    },
    /// `SecurityEventNotification`: what the station's security monitor saw.
    Security {
        /// The event type, e.g. `TamperDetectionActivated`.
        event: String,
        /// The station's own detail, bounded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

/// A wire enum as the word the station sent.
pub(crate) fn word<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => clean(&text),
        _ => String::new(),
    }
}

/// A security event, bounded.
#[must_use]
pub fn security(event: &str, detail: Option<&String>) -> Report {
    Report::Security {
        event: clean(event),
        detail: detail.map(|d| d.chars().filter(|c| !c.is_control()).take(255).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sr_137_a_station_string_is_bounded_and_has_no_control_characters() {
        let long = format!("Model\u{0007}\n{}", "x".repeat(80));
        let cleaned = clean(&long);
        assert!(cleaned.chars().count() <= MAX_LEN);
        assert!(!cleaned.chars().any(char::is_control), "{cleaned:?}");
        assert!(cleaned.starts_with("Model"));
    }

    #[test]
    fn sr_149_sr_150_an_imsi_is_kept_as_its_network_only() {
        let imsi = |s: &str| network(Some(&s.to_owned()));
        assert_eq!(imsi("262011234567890").as_deref(), Some("26201"));
        assert_eq!(
            imsi("310260123456789").as_deref(),
            Some("310260"),
            "3-digit MNC"
        );
        assert_eq!(imsi("26201abc4567890"), None, "digits only");
        assert_eq!(imsi("2620"), None, "too short to be an IMSI");
        assert_eq!(imsi("2620112345678901"), None, "too long to be an IMSI");
        assert_eq!(network(None), None);
    }
}
