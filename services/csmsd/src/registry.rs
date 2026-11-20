//! The station registry (spec 021): who may connect, and as what.
//!
//! A station is the estate's own [`Station`] record, journalled as a fact —
//! registered, updated, retired — under whoever caused it. The estate's
//! `[[stations]]` seed the registry at start; an operator adds the rest. Every
//! record is checked here, once, by the rules an estate station is checked by,
//! before anything is journalled (SR-145); admission then reads the registry
//! on every connection (SR-146).
//!
//! A password is minted here and leaves in exactly one response: the record
//! holds its Argon2id hash and nothing else (SR-144).

use std::collections::BTreeMap;

use emob_service::estate::{SecurityProfile, Station};
use emob_tariff::Tariff;
use ocpp_kit::types::Identity;
use serde::Serialize;

use crate::provisioning::ChargePoint;

/// Why a record cannot be registered — one field, one sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{field}: {reason}")]
pub struct Refusal {
    /// The field, as the form and the estate name it.
    pub field: String,
    /// What is wrong, and what would be valid.
    pub reason: String,
}

fn refuse(field: &str, reason: impl Into<String>) -> Refusal {
    Refusal {
        field: field.to_owned(),
        reason: reason.into(),
    }
}

/// Check a record against the registry and the tariffs it may name.
///
/// `replacing` is the identity an update replaces, which may keep its own.
///
/// # Errors
///
/// The first [`Refusal`].
pub fn check(
    station: &Station,
    registry: &BTreeMap<String, Station>,
    tariffs: &BTreeMap<String, Tariff>,
    replacing: Option<&str>,
) -> Result<(), Refusal> {
    Identity::new(&station.identity).map_err(|e| refuse("identity", e.to_string()))?;
    emob_service::Actor::station(&station.identity)
        .map_err(|e| refuse("identity", e.to_string()))?;
    if replacing != Some(station.identity.as_str()) && registry.contains_key(&station.identity) {
        return Err(refuse(
            "identity",
            format!("{} is already registered", station.identity),
        ));
    }
    if replacing.is_none() && registry.values().any(|other| other.evse == station.evse) {
        return Err(refuse(
            "evse",
            format!("{} is already a registered station's point", station.evse),
        ));
    }
    if !tariffs.contains_key(&station.tariff) {
        let known: Vec<&str> = tariffs.keys().map(String::as_str).collect();
        return Err(refuse(
            "tariff",
            format!(
                "no tariff has the id {:?}; the estate's tariffs are {}",
                station.tariff,
                known.join(", ")
            ),
        ));
    }
    let country = &station.location.country;
    if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(refuse(
            "location.country",
            format!("{country:?} is not an ISO 3166-1 alpha-2 code like DE"),
        ));
    }
    emob_core::TimeZone::new(&station.location.time_zone)
        .map_err(|e| refuse("location.time_zone", e.to_string()))?;
    if let Some(minutes) = station.receipt_display_minutes
        && !(1..=crate::display::MAX_MINUTES).contains(&minutes)
    {
        return Err(refuse(
            "receipt_display_minutes",
            format!("{minutes} is not 1–{}", crate::display::MAX_MINUTES),
        ));
    }
    if let Some(day) = &station.commissioned {
        time::Date::parse(day, &time::format_description::well_known::Iso8601::DATE).map_err(
            |_| {
                refuse(
                    "commissioned",
                    format!("{day} is not a date like 2024-05-02"),
                )
            },
        )?;
    }
    match (station.security_profile, &station.password_hash) {
        (SecurityProfile::Basic | SecurityProfile::TlsBasic, None) => Err(refuse(
            "password_hash",
            "security profiles 1 and 2 authenticate with a password",
        )),
        (SecurityProfile::TlsClientCertificate, Some(_)) => Err(refuse(
            "password_hash",
            "security profile 3 authenticates with a certificate, not a password",
        )),
        _ => Ok(()),
    }
}

/// A fresh station password and its hash: 20 random bytes as 40 hex digits —
/// a valid OCPP 1.6 `AuthorizationKey` and a valid 2.x Basic password.
///
/// # Errors
///
/// When the system's random source or the hash fails.
pub fn mint(station: &Station) -> Result<(String, String), Refusal> {
    let mut bytes = [0_u8; 20];
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| refuse("password", "the system RNG failed"))?;
    let password: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let hash = crate::edge::hash_password(crate::estate::version(station.ocpp), &password)
        .map_err(|e| refuse("password", e))?;
    Ok((password, hash))
}

/// What admission and pricing need from a registered station.
#[must_use]
pub fn charge_point(station: &Station, tariffs: &BTreeMap<String, Tariff>) -> Option<ChargePoint> {
    Some(ChargePoint {
        evse_id: station.evse.clone(),
        rated_power_kw: station.rated_power_kw,
        tariff: tariffs.get(&station.tariff)?.clone(),
        ocpp: crate::estate::version(station.ocpp),
    })
}
