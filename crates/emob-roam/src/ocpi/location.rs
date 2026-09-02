//! Where the session happened, from the register that publishes it.
//!
//! # One inventory, two audiences
//!
//! A CPO states where a charge point is twice: to a roaming partner, in an
//! OCPI `CdrLocation` and the Locations module, and to the public, in the
//! national access point feed `[AFIR Art. 20(2)(c)]`. Almost every stack
//! generates the two from different systems, and the drift is invisible
//! because nobody ever compares them — a connector retyped from CCS to
//! `CHAdeMO` in one and not the other produces a partner whose app sends drivers
//! to a plug that is not there, while the Mobilithek shows the right one.
//!
//! That is the same argument [`emob_poi`] makes about the price, one field
//! over, so it gets the same answer: this module has no location model. It
//! reads [`emob_poi::site`], which is what the DATEX II publication is built
//! from.
//!
//! # The lengths are the specification's, and they are not advisory
//!
//! OCPI bounds `address` at 45 characters and `city` at 45
//! `[OCPI 2.3.0 §mod_cdrs_cdrlocation_class]`. A German street and house
//! number reach that more often than anyone expects, and the usual handling —
//! truncate and move on — produces a record naming an address that does not
//! exist. It is refused instead.

use emob_core::CurrentType;
use emob_poi::site::{ChargingPoint, Connector, ConnectorType, Site};
use ocpi_kit::types::{CiString, OcpiString};
use ocpi_kit::v2_3_0::cdrs::CdrLocation;
use ocpi_kit::v2_3_0::locations::{
    ConnectorFormat, ConnectorType as OcpiConnector, GeoLocation, PowerType,
};

use crate::error::RoamError;

/// The OCPI spelling of a connector.
///
/// The two vocabularies were written from the same IEC standard, so every
/// variant the register carries today has one. `None` is for the variant that
/// arrives later: [`ConnectorType`] is `#[non_exhaustive]`, and a crossing
/// that fell back to the nearest plug would publish a socket that is not
/// there. A refusal is a job for an operator; a wrong plug is a driver at the
/// wrong charger.
#[must_use]
pub fn connector_type(kind: ConnectorType) -> Option<OcpiConnector> {
    Some(match kind {
        ConnectorType::Iec62196T2 => OcpiConnector::Iec62196T2,
        ConnectorType::Iec62196T2Combo => OcpiConnector::Iec62196T2Combo,
        ConnectorType::Iec62196T1 => OcpiConnector::Iec62196T1,
        ConnectorType::Iec62196T1Combo => OcpiConnector::Iec62196T1Combo,
        ConnectorType::Chademo => OcpiConnector::Chademo,
        ConnectorType::DomesticF => OcpiConnector::DomesticF,
        ConnectorType::Cee5 => OcpiConnector::Iec603092Three16,
        _ => return None,
    })
}

/// Whether the point delivers AC or DC, in OCPI's spelling.
///
/// OCPI splits AC by phase count and `emob_poi` does not carry one, so an AC
/// point crosses as three-phase — which is what a European public AC point
/// above 3.7 kW is, and the figure that matters for a driver's decision, the
/// power, is carried exactly beside it.
#[must_use]
pub fn power_type(point: &ChargingPoint) -> PowerType {
    match point.current_type {
        CurrentType::Dc => PowerType::Dc,
        CurrentType::Ac => PowerType::Ac3Phase,
    }
}

/// The `CdrLocation` for one connector of one point.
///
/// # Errors
///
/// [`RoamError::TooLong`] when a field the register holds does not fit the
/// bound OCPI puts on it, [`RoamError::InvalidString`] when it carries a
/// character OCPI does not allow, and [`RoamError::UnmappedConnector`] for a
/// plug OCPI has no name for. None of the three is repaired: a truncated
/// address names a different building, and the nearest plug is the wrong one.
pub fn cdr_location(
    site: &Site,
    point: &ChargingPoint,
    connector: &Connector,
    connector_index: usize,
) -> Result<CdrLocation, RoamError> {
    let street = format!("{} {}", site.address.street, site.address.house_number);

    Ok(CdrLocation::builder()
        .id(bounded::<36>("cdr_location.id", &site.facility.id)?)
        .name(bounded_ocpi::<255>("cdr_location.name", &site.name)?)
        .address(bounded_ocpi::<45>("cdr_location.address", &street)?)
        .city(bounded_ocpi::<45>("cdr_location.city", &site.address.city)?)
        .postal_code(bounded_ocpi::<10>(
            "cdr_location.postal_code",
            &site.address.postcode,
        )?)
        .country(bounded_ocpi::<3>(
            "cdr_location.country",
            &alpha3(&site.address.country_code),
        )?)
        .coordinates(
            GeoLocation::new(
                site.coordinates.latitude.to_string(),
                site.coordinates.longitude.to_string(),
            )
            .map_err(|source| RoamError::InvalidString {
                field: "cdr_location.coordinates",
                source,
            })?,
        )
        // OCPI's `evse_uid` is the CPO's own handle for the EVSE and its
        // `evse_id` is the public one. `emob_poi` carries the point's facility
        // id for exactly the first purpose — it is what the DATEX publication
        // addresses the point by — so the two documents name the same thing.
        .evse_uid(bounded::<36>("cdr_location.evse_uid", &point.facility.id)?)
        .evse_id(bounded::<48>(
            "cdr_location.evse_id",
            &point.evse_id.to_string(),
        )?)
        .connector_id(bounded::<36>(
            "cdr_location.connector_id",
            &connector_index.to_string(),
        )?)
        .connector_standard(connector_type(connector.kind).ok_or_else(|| {
            RoamError::UnmappedConnector {
                kind: connector.kind.as_profile_str().to_owned(),
            }
        })?)
        .connector_format(ConnectorFormat::Socket)
        .connector_power_type(power_type(point))
        .build())
}

/// ISO 3166-1 alpha-3, which OCPI asks for where the register holds alpha-2.
///
/// Only the countries a European charging network is actually operated in are
/// mapped; anything else crosses as it arrived and fails the length bound if
/// it is not three characters, which is a refusal rather than a wrong country.
fn alpha3(alpha2: &str) -> String {
    match alpha2.to_ascii_uppercase().as_str() {
        "DE" => "DEU",
        "AT" => "AUT",
        "CH" => "CHE",
        "NL" => "NLD",
        "BE" => "BEL",
        "FR" => "FRA",
        "LU" => "LUX",
        "DK" => "DNK",
        "PL" => "POL",
        "CZ" => "CZE",
        "IT" => "ITA",
        "ES" => "ESP",
        other => return other.to_owned(),
    }
    .to_owned()
}

/// A case-insensitive string that fits its bound, or a refusal that names it.
pub(crate) fn bounded<const N: usize>(
    field: &'static str,
    value: &str,
) -> Result<CiString<N>, RoamError> {
    CiString::new(value).map_err(|source| length_or_charset::<N>(field, value, source))
}

/// A printable string that fits its bound, or a refusal that names it.
pub(crate) fn bounded_ocpi<const N: usize>(
    field: &'static str,
    value: &str,
) -> Result<OcpiString<N>, RoamError> {
    OcpiString::new(value).map_err(|source| length_or_charset::<N>(field, value, source))
}

/// Tell "too long" from "not printable", because the two have different fixes.
fn length_or_charset<const N: usize>(
    field: &'static str,
    value: &str,
    source: ocpi_kit::types::InvalidString,
) -> RoamError {
    if value.chars().count() > N {
        RoamError::TooLong {
            field,
            len: value.chars().count(),
            max: N,
        }
    } else {
        RoamError::InvalidString { field, source }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_poi::site::{Address, Coordinates, Facility};
    use emob_poi::status::Lifecycle;
    use rust_decimal::Decimal;
    use rust_decimal::prelude::FromStr;

    fn site() -> Site {
        Site {
            facility: Facility::new("site-1"),
            name: "Autohof Nord".to_owned(),
            coordinates: Coordinates {
                latitude: Decimal::from_str("52.520008").unwrap(),
                longitude: Decimal::from_str("13.404954").unwrap(),
            },
            address: Address {
                street: "Kurfürstendamm".to_owned(),
                house_number: "12".to_owned(),
                postcode: "10719".to_owned(),
                city: "Berlin".to_owned(),
                country_code: "DE".to_owned(),
            },
            time_zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            stations: Vec::new(),
        }
    }

    fn point() -> ChargingPoint {
        let mut point = ChargingPoint::new(
            Facility::new("evse-1"),
            "DE*ABC*E840*6487".parse().unwrap(),
            Connector::new(ConnectorType::Iec62196T2Combo, Decimal::from(150)),
        );
        point.lifecycle = Lifecycle::Operating;
        point
    }

    #[test]
    fn the_location_a_partner_reads_is_the_one_the_feed_publishes() {
        let point = point();
        let location = cdr_location(&site(), &point, &point.connectors[0], 0).unwrap();

        assert_eq!(location.evse_id.as_str(), "DE*ABC*E840*6487");
        assert_eq!(location.evse_uid.as_str(), "evse-1");
        assert_eq!(location.city.as_str(), "Berlin");
        assert_eq!(location.country.as_str(), "DEU");
        assert_eq!(location.connector_standard, OcpiConnector::Iec62196T2Combo);
        assert_eq!(location.connector_power_type, PowerType::Dc);
        assert_eq!(location.coordinates.latitude.as_str(), "52.520008");
    }

    #[test]
    fn an_address_that_does_not_fit_is_refused_rather_than_truncated() {
        // 45 characters is not a generous bound for a German street.
        let mut long = site();
        long.address.street = "Straße des Siebzehnten Juni am Großen Stern".to_owned();
        long.address.house_number = "135a".to_owned();

        let point = point();
        let err = cdr_location(&long, &point, &point.connectors[0], 0).unwrap_err();
        assert!(
            matches!(
                err,
                RoamError::TooLong {
                    field: "cdr_location.address",
                    max: 45,
                    ..
                }
            ),
            "a truncated address names a different building: {err}"
        );
    }

    #[test]
    fn an_ac_point_crosses_as_three_phase_and_keeps_its_exact_power() {
        let mut ac = point();
        ac.current_type = CurrentType::Ac;
        ac.connectors = vec![Connector::new(ConnectorType::Iec62196T2, Decimal::from(22))];
        assert_eq!(power_type(&ac), PowerType::Ac3Phase);
    }
}
