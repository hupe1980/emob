//! The sections of `estate.toml` `csmsd` reads, and what they become.
//!
//! The stations, the meter keys and the tariffs are the three bindings a station
//! may not supply for itself; here they come from the estate and nowhere else.
//! A value a domain crate refuses is refused here naming `section[index].field`.

use std::collections::BTreeMap;

use emob_eichrecht::KeyRegistry;
use emob_eichrecht::registry::{ComponentRef, RegisteredKey};
use emob_service::estate::{
    Csms, EstateError, MeterKey, Ocpp, Operator, Parties, Services, Station, TariffSpec,
};
use ocpp_kit::Version;
use ocpp_kit::types::Identity;
use serde::Deserialize;

use crate::{ChargePoint, Provisioning};

/// What `csmsd` reads of the estate.
#[derive(Debug, Clone, Deserialize)]
pub struct Estate {
    /// Whom the CDRs are issued under.
    pub parties: Parties,
    /// The fleet.
    pub stations: Vec<Station>,
    /// The keys that make energy billable. Empty is allowed and keeps the
    /// daemon unready (SR-005).
    #[serde(default)]
    pub meter_keys: Vec<MeterKey>,
    /// The tariffs stations name.
    pub tariffs: Vec<TariffSpec>,
    /// Who may read the read model.
    #[serde(default)]
    pub operators: Vec<Operator>,
    /// Where `empd`, `billd` and `roamd` are.
    pub services: Services,
    /// How the OCPP listener is exposed; loopback without TLS when absent.
    #[serde(default)]
    pub csms: Csms,
    /// Where receipt links point (spec 003).
    pub receipts: ReceiptSection,
    /// The one key of the operator page's section `csmsd` reads.
    pub desk: DeskZone,
    /// Systems outside the estate that receive events.
    #[serde(default)]
    pub subscribers: Vec<emob_service::estate::Subscriber>,
}

/// `[desk] time_zone` — the zone the operator page tells "today" in, which is
/// the zone an attempt is counted on its day in (spec 007). The rest of
/// `[desk]` is `deskd`'s.
#[derive(Debug, Clone, Deserialize)]
pub struct DeskZone {
    /// The IANA zone.
    pub time_zone: String,
}

/// The estate's zone.
///
/// # Errors
///
/// [`EstateError::Invalid`] naming `desk.time_zone` for a zone the compiled-in
/// database does not know.
pub fn zone(estate: &Estate) -> Result<emob_core::TimeZone, EstateError> {
    emob_core::TimeZone::new(&estate.desk.time_zone)
        .map_err(|e| invalid("desk.time_zone".to_owned(), e))
}

/// `[receipts]` — read by `csmsd` only.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptSection {
    /// The origin a driver reaches the shell at; links are
    /// `<public_url>/r/<token>`.
    pub public_url: String,
}

fn invalid(key: String, reason: impl ToString) -> EstateError {
    EstateError::Invalid {
        key,
        reason: reason.to_string(),
    }
}

/// The OCPP version a station was provisioned for.
#[must_use]
pub const fn version(ocpp: Ocpp) -> Version {
    match ocpp {
        Ocpp::V1_6 => Version::V1_6,
        Ocpp::V2_0_1 => Version::V2_0_1,
        Ocpp::V2_1 => Version::V2_1,
    }
}

/// The fleet: identity → charge point with its tariff.
///
/// # Errors
///
/// [`EstateError::Invalid`] for an identity OCPP cannot carry, a tariff no
/// `[[tariffs]]` entry defines, or a station listed twice.
pub fn provisioning(estate: &Estate) -> Result<Provisioning, EstateError> {
    let mut tariffs = BTreeMap::new();
    for (index, spec) in estate.tariffs.iter().enumerate() {
        tariffs.insert(spec.id.clone(), spec.to_tariff(index)?);
    }
    let mut fleet = Provisioning::new();
    for (index, station) in estate.stations.iter().enumerate() {
        let identity = Identity::new(&station.identity)
            .map_err(|e| invalid(format!("stations[{index}].identity"), e))?;
        // Every fact the station causes is journalled under its identity, so
        // one that is not an actor would be a station whose facts nobody can
        // attribute (SR-101).
        emob_service::Actor::station(&station.identity)
            .map_err(|e| invalid(format!("stations[{index}].identity"), e))?;
        if fleet.knows(&identity) {
            return Err(invalid(
                format!("stations[{index}].identity"),
                format!("{} is listed twice", station.identity),
            ));
        }
        let tariff = tariffs.get(&station.tariff).cloned().ok_or_else(|| {
            invalid(
                format!("stations[{index}].tariff"),
                format!("no [[tariffs]] entry has the id {:?}", station.tariff),
            )
        })?;
        fleet = fleet.with(
            identity,
            ChargePoint {
                evse_id: station.evse.clone(),
                rated_power_kw: station.rated_power_kw,
                tariff,
                ocpp: version(station.ocpp),
            },
        );
    }
    Ok(fleet)
}

/// The key registry, each key with the provenance the estate states.
///
/// # Errors
///
/// [`EstateError::Invalid`] naming `meter_keys[index].<field>` for a curve
/// `[OCMF Tab. 23]` does not name, a key that does not decode, a window that is
/// not RFC 3339, or two keys for one component whose windows overlap.
pub fn registry(keys: &[MeterKey]) -> Result<KeyRegistry, EstateError> {
    let mut registry = KeyRegistry::new();
    for (index, entry) in keys.iter().enumerate() {
        let at = |field: &str| format!("meter_keys[{index}].{field}");
        let curve = ocmf::Curve::ALL
            .into_iter()
            .find(|curve| curve.name() == entry.curve)
            .ok_or_else(|| invalid(at("curve"), format!("{} is not an OCMF curve", entry.curve)))?;
        let key = ocmf::PublicKey::from_text(&entry.key, Some(curve))
            .map_err(|e| invalid(at("key"), e))?;
        let instant = |field: &str, value: Option<&String>| {
            value
                .map(|text| {
                    time::OffsetDateTime::parse(
                        text,
                        &time::format_description::well_known::Rfc3339,
                    )
                    .map_err(|e| invalid(at(field), e))
                })
                .transpose()
        };
        let registered = RegisteredKey {
            key,
            valid_from: instant("valid_from", entry.valid_from.as_ref())?,
            valid_until: instant("valid_until", entry.valid_until.as_ref())?,
            provenance: entry.provenance.clone(),
        };
        let component = match &entry.gateway {
            Some(gateway) => ComponentRef::GatewayAndMeter {
                gateway: gateway.clone(),
                meter: entry.meter.clone(),
            },
            None => ComponentRef::Meter {
                serial: entry.meter.clone(),
            },
        };
        registry
            .insert(component, registered)
            .map_err(|e| invalid(at("key"), e))?;
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> Estate {
        emob_service::estate::from_str(include_str!("../../../deploy/estate.example.toml"))
            .expect("the demo estate")
    }

    #[test]
    fn the_demo_estate_provisions_its_station_with_its_tariff_and_key() {
        let estate = demo();
        let fleet = provisioning(&estate).unwrap();
        let point = fleet.get(&Identity::new("CP-1").unwrap()).unwrap();
        assert_eq!(point.evse_id.to_string(), "DE*ABC*E00001");
        assert_eq!(point.ocpp, Version::V1_6);
        assert_eq!(registry(&estate.meter_keys).unwrap().len(), 1);
    }

    #[test]
    fn sr_036_a_station_naming_a_tariff_nobody_defined_is_refused_by_key() {
        let mut estate = demo();
        estate.stations[0].tariff = "night".to_owned();
        let error = provisioning(&estate).expect_err("no such tariff");
        assert!(error.to_string().contains("stations[0].tariff"), "{error}");
    }

    #[test]
    fn the_day_is_read_in_the_desk_zone_and_an_unknown_zone_is_refused_by_key() {
        let mut estate = demo();
        assert_eq!(zone(&estate).unwrap().name(), "Europe/Berlin");
        estate.desk.time_zone = "Europe/Atlantis".to_owned();
        let error = zone(&estate).expect_err("no such zone");
        assert!(error.to_string().contains("desk.time_zone"), "{error}");
    }

    #[test]
    fn sr_036_a_meter_key_that_does_not_decode_is_refused_by_key() {
        let mut estate = demo();
        estate.meter_keys[0].key = "04deadbeef".to_owned();
        let error = registry(&estate.meter_keys).expect_err("not a point");
        assert!(error.to_string().contains("meter_keys[0].key"), "{error}");

        estate.meter_keys[0].curve = "curve25519".to_owned();
        let error = registry(&estate.meter_keys).expect_err("not an OCMF curve");
        assert!(error.to_string().contains("meter_keys[0].curve"), "{error}");
    }
}
