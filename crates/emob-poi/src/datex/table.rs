//! `EnergyInfrastructureTablePublication` — the static half of the feed.
//!
//! What `[AFIR Art. 20(2)(a)–(b)]` calls static data: where the points are,
//! what they are, what they cost. It changes when somebody sends an engineer,
//! and the dynamic publication addresses it by `idG` and `versionG` — so
//! republishing a table with a bumped version and not telling the status feed
//! is how a working status feed stops being delivered. [`crate::feed`] is where
//! the two are checked against each other.

#![allow(
    clippy::struct_field_names,
    reason = "the fields are named for the profile's own attributes, and a Rust \
              name that differs from the JSON key it writes is the one mistake \
              these structs exist to prevent"
)]

use serde::Serialize;

use emob_core::CurrentType;

use super::wire::{Enumerated, Exact, Extended, Multilingual, time_of_day, timestamp, watts};
use crate::rate::{Period, Price, Rate};
use crate::site::{ChargingPoint, Connector, Site, Station};

/// The DATEX II model version the profile extends.
const MODEL_BASE_VERSION: &str = "3";
/// The profile's own name, as it appears in every published example.
const PROFILE_NAME: &str = "AFIR Energy Infrastructure";
/// The profile version this module writes `[DATEX-II-Profil]`.
pub const PROFILE_VERSION: &str = "01-00-00";

/// Who is publishing, and under what national identifier.
///
/// The Mobilithek issues the `nationalIdentifier`; it is not something a
/// producer chooses, which is why it is an argument rather than a default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publisher {
    /// ISO 3166-1 alpha-2 of the publishing country.
    pub country: String,
    /// The identifier the national access point issued.
    pub national_identifier: String,
    /// The language the human-readable strings are written in.
    pub language: String,
}

/// Whether a publication describes reality or a test.
///
/// `informationStatus` is not decoration: a consumer routes on `real` and is
/// expected to discard `test`. A production feed published as `test` is
/// invisible; a test feed published as `real` sends drivers to a fiction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InformationStatus {
    /// Describes infrastructure that exists.
    Real,
    /// A test publication.
    Test,
}

impl InformationStatus {
    pub(crate) const fn as_profile_str(self) -> &'static str {
        match self {
            Self::Real => "real",
            Self::Test => "test",
        }
    }
}

/// The static publication.
#[derive(Debug, Clone, Serialize)]
pub struct TablePublication {
    payload: Payload,
}

#[derive(Debug, Clone, Serialize)]
struct Payload {
    #[serde(rename = "modelBaseVersionG")]
    model_base_version: &'static str,
    #[serde(rename = "profileNameG")]
    profile_name: &'static str,
    #[serde(rename = "profileVersionG")]
    profile_version: &'static str,
    #[serde(rename = "aegiEnergyInfrastructureTablePublication")]
    publication: Publication,
}

#[derive(Debug, Clone, Serialize)]
struct Publication {
    lang: String,
    #[serde(rename = "publicationTime")]
    publication_time: String,
    #[serde(rename = "publicationCreator")]
    publication_creator: Creator,
    #[serde(rename = "headerInformation")]
    header_information: Header,
    #[serde(rename = "energyInfrastructureTable")]
    tables: Vec<Table>,
}

#[derive(Debug, Clone, Serialize)]
struct Creator {
    country: String,
    #[serde(rename = "nationalIdentifier")]
    national_identifier: String,
}

#[derive(Debug, Clone, Serialize)]
struct Header {
    confidentiality: Enumerated,
    #[serde(rename = "informationStatus")]
    information_status: Enumerated,
}

#[derive(Debug, Clone, Serialize)]
struct Table {
    #[serde(rename = "idG")]
    id: String,
    #[serde(rename = "versionG")]
    version: String,
    #[serde(rename = "tableName", skip_serializing_if = "Option::is_none")]
    table_name: Option<String>,
    #[serde(rename = "energyInfrastructureSite")]
    sites: Vec<SiteOut>,
}

#[derive(Debug, Clone, Serialize)]
struct SiteOut {
    #[serde(rename = "idG")]
    id: String,
    #[serde(rename = "versionG")]
    version: String,
    #[serde(rename = "lastUpdated")]
    last_updated: String,
    name: Multilingual,
    #[serde(rename = "locationReference")]
    location_reference: LocationReference,
    #[serde(rename = "energyInfrastructureStation")]
    stations: Vec<StationOut>,
}

#[derive(Debug, Clone, Serialize)]
struct LocationReference {
    #[serde(rename = "locAreaLocation")]
    area: AreaLocation,
}

#[derive(Debug, Clone, Serialize)]
struct AreaLocation {
    #[serde(rename = "coordinatesForDisplay")]
    coordinates: Coordinates,
    #[serde(rename = "locLocationExtensionG")]
    extension: LocationExtension,
}

#[derive(Debug, Clone, Serialize)]
struct Coordinates {
    latitude: Exact,
    longitude: Exact,
}

#[derive(Debug, Clone, Serialize)]
struct LocationExtension {
    #[serde(rename = "FacilityLocation")]
    facility_location: FacilityLocation,
}

#[derive(Debug, Clone, Serialize)]
struct FacilityLocation {
    #[serde(rename = "timeZone")]
    time_zone: String,
    address: Address,
}

#[derive(Debug, Clone, Serialize)]
struct Address {
    postcode: String,
    city: Multilingual,
    #[serde(rename = "countryCode")]
    country_code: String,
    #[serde(rename = "addressLine")]
    address_line: Vec<AddressLine>,
}

#[derive(Debug, Clone, Serialize)]
struct AddressLine {
    order: u32,
    #[serde(rename = "type")]
    kind: Enumerated,
    text: Multilingual,
}

#[derive(Debug, Clone, Serialize)]
struct StationOut {
    #[serde(rename = "idG")]
    id: String,
    #[serde(rename = "versionG")]
    version: String,
    #[serde(rename = "lastUpdated")]
    last_updated: String,
    operator: OperatorOut,
    #[serde(rename = "authenticationAndIdentificationMethods")]
    authentication: Vec<Enumerated>,
    #[serde(rename = "numberOfRefillPoints")]
    number_of_refill_points: usize,
    #[serde(rename = "totalMaximumPower")]
    total_maximum_power: Exact,
    #[serde(rename = "refillPoint")]
    refill_points: Vec<RefillPointOut>,
}

#[derive(Debug, Clone, Serialize)]
struct OperatorOut {
    #[serde(rename = "afacAnOrganisation")]
    organisation: Organisation,
}

#[derive(Debug, Clone, Serialize)]
struct Organisation {
    name: Multilingual,
}

#[derive(Debug, Clone, Serialize)]
struct RefillPointOut {
    #[serde(rename = "aegiElectricChargingPoint")]
    point: ElectricChargingPoint,
}

#[derive(Debug, Clone, Serialize)]
struct ElectricChargingPoint {
    #[serde(rename = "idG")]
    id: String,
    #[serde(rename = "versionG")]
    version: String,
    #[serde(rename = "lastUpdated")]
    last_updated: String,
    #[serde(rename = "externalIdentifier")]
    external_identifier: Vec<ExternalIdentifier>,
    #[serde(rename = "deliveryUnit")]
    delivery_unit: Enumerated,
    #[serde(rename = "currentType")]
    current_type: Enumerated,
    #[serde(rename = "vehicleToGridCommunicationType")]
    v2g: Vec<Enumerated>,
    #[serde(rename = "numberOfConnectors")]
    number_of_connectors: usize,
    #[serde(rename = "availableChargingPower")]
    available_charging_power: Vec<Exact>,
    connector: Vec<ConnectorOut>,
    #[serde(rename = "electricEnergy", skip_serializing_if = "Vec::is_empty")]
    electric_energy: Vec<ElectricEnergy>,
}

#[derive(Debug, Clone, Serialize)]
struct ExternalIdentifier {
    identifier: String,
    #[serde(rename = "typeOfIdentifier")]
    type_of_identifier: Extended,
}

#[derive(Debug, Clone, Serialize)]
struct ConnectorOut {
    #[serde(rename = "connectorType")]
    connector_type: Enumerated,
    #[serde(rename = "maxPowerAtSocket")]
    max_power_at_socket: Exact,
}

#[derive(Debug, Clone, Serialize)]
struct ElectricEnergy {
    #[serde(rename = "energyRate")]
    energy_rate: Vec<EnergyRate>,
}

#[derive(Debug, Clone, Serialize)]
struct EnergyRate {
    #[serde(rename = "idG")]
    id: String,
    #[serde(rename = "ratePolicy")]
    rate_policy: Enumerated,
    #[serde(rename = "lastUpdated")]
    last_updated: String,
    #[serde(rename = "applicableCurrency")]
    applicable_currency: Vec<String>,
    #[serde(rename = "rateName", skip_serializing_if = "Option::is_none")]
    rate_name: Option<Multilingual>,
    #[serde(
        rename = "combinationWithParkingFee",
        skip_serializing_if = "Option::is_none"
    )]
    combination_with_parking_fee: Option<bool>,
    #[serde(rename = "minimumDeliveryFee", skip_serializing_if = "Option::is_none")]
    minimum_delivery_fee: Option<Exact>,
    #[serde(rename = "maximumDeliveryFee", skip_serializing_if = "Option::is_none")]
    maximum_delivery_fee: Option<Exact>,
    #[serde(rename = "energyPrice")]
    energy_price: Vec<EnergyPrice>,
}

#[derive(Debug, Clone, Serialize)]
struct EnergyPrice {
    #[serde(rename = "priceType")]
    price_type: Enumerated,
    value: Exact,
    #[serde(rename = "taxIncluded", skip_serializing_if = "Option::is_none")]
    tax_included: Option<bool>,
    #[serde(rename = "taxRate", skip_serializing_if = "Option::is_none")]
    tax_rate: Option<Exact>,
    #[serde(
        rename = "additionalInformation",
        skip_serializing_if = "Option::is_none"
    )]
    additional_information: Option<Multilingual>,
    #[serde(rename = "overallPeriod", skip_serializing_if = "Option::is_none")]
    overall_period: Option<OverallPeriod>,
    #[serde(
        rename = "energyBasedApplicability",
        skip_serializing_if = "Option::is_none"
    )]
    energy_based_applicability: Option<EnergyBasedApplicability>,
    #[serde(
        rename = "timeBasedApplicability",
        skip_serializing_if = "Option::is_none"
    )]
    time_based_applicability: Option<TimeBasedApplicability>,
}

/// The delivered-energy band a price applies in — the profile's own answer to
/// a tiered tariff, in whole kilowatt-hours.
#[derive(Debug, Clone, Serialize)]
struct EnergyBasedApplicability {
    #[serde(rename = "fromKWh", skip_serializing_if = "Option::is_none")]
    from_kwh: Option<u32>,
    #[serde(rename = "toKWh", skip_serializing_if = "Option::is_none")]
    to_kwh: Option<u32>,
}

/// The elapsed-time band, in whole minutes.
#[derive(Debug, Clone, Serialize)]
struct TimeBasedApplicability {
    #[serde(rename = "fromMinute", skip_serializing_if = "Option::is_none")]
    from_minute: Option<u32>,
    #[serde(rename = "toMinute", skip_serializing_if = "Option::is_none")]
    to_minute: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct OverallPeriod {
    #[serde(rename = "overallStartTime", skip_serializing_if = "Option::is_none")]
    start: Option<String>,
    #[serde(rename = "overallEndTime", skip_serializing_if = "Option::is_none")]
    end: Option<String>,
    #[serde(rename = "validPeriod", skip_serializing_if = "Vec::is_empty")]
    valid_period: Vec<ValidPeriod>,
}

#[derive(Debug, Clone, Serialize)]
struct ValidPeriod {
    #[serde(rename = "recurringTimePeriodOfDay")]
    recurring: Vec<TimePeriodOfDay>,
}

#[derive(Debug, Clone, Serialize)]
struct TimePeriodOfDay {
    #[serde(rename = "startTimeOfPeriod")]
    start: String,
    #[serde(rename = "endTimeOfPeriod")]
    end: String,
}

/// Build the static publication for one table of sites.
///
/// `published_at` is an argument rather than a clock reading: this crate has no
/// I/O and no clock, which is what lets an export be replayed years later and
/// produce the same bytes `[MessEG §33]`. It becomes both `publicationTime` and
/// every object's `lastUpdated`.
///
/// `rate_for` supplies the published prices for a point. Returning `None` is
/// admissible — a point behind a contract-only site has no ad-hoc rate to
/// publish — but a **publicly accessible** point without one is a point whose
/// ad-hoc price `[AFIR Art. 20(2)(c)]` requires and this feed does not carry.
#[must_use]
pub fn publication(
    publisher: &Publisher,
    status: InformationStatus,
    table: &crate::site::Facility,
    table_name: Option<&str>,
    sites: &[Site],
    published_at: time::OffsetDateTime,
    rate_for: &dyn Fn(&ChargingPoint) -> Option<Rate>,
) -> TablePublication {
    let now = timestamp(published_at);
    TablePublication {
        payload: Payload {
            model_base_version: MODEL_BASE_VERSION,
            profile_name: PROFILE_NAME,
            profile_version: PROFILE_VERSION,
            publication: Publication {
                lang: publisher.language.clone(),
                publication_time: now.clone(),
                publication_creator: Creator {
                    country: publisher.country.clone(),
                    national_identifier: publisher.national_identifier.clone(),
                },
                header_information: Header {
                    confidentiality: Enumerated::new("noRestriction"),
                    information_status: Enumerated::new(status.as_profile_str()),
                },
                tables: vec![Table {
                    id: table.id.clone(),
                    version: table.version.to_string(),
                    table_name: table_name.map(ToOwned::to_owned),
                    sites: sites
                        .iter()
                        .map(|site| {
                            site_of(site, &publisher.language, &now, published_at, rate_for)
                        })
                        .collect(),
                }],
            },
        },
    }
}

impl TablePublication {
    /// The publication as the JSON the national access point ingests.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] only if a value cannot be represented, which the
    /// types here prevent.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// The site's zone, in the only spelling `[DATEX-II-Profil]` accepts.
///
/// # The profile asks for an offset where the fact is a zone
///
/// `FacilityLocation.timeZone` is typed as a string that "identifies a time zone
/// by specifying the difference to UTC in hours and minutes, as defined in
/// ISO 8601", and the profile's own reference instance publishes `"+01:00"` for
/// a site in Aachen. An ISO 8601 offset cannot express a zone that observes
/// summer time: `+01:00` is wrong for that site from the last Sunday in March
/// to the last Sunday in October, and it is the field a consumer would read the
/// crate's own daily price windows against.
///
/// There is no honest fixed value, so this publishes the offset **in force at
/// the moment of publication** — the one reading that is true of the document
/// when it is issued — and [`crate::rate::RateNote::TimeZoneIsAnOffset`] says
/// so beside it. Republishing the table across a clock change republishes the
/// right offset, which is the behaviour the profile's shape forces on anybody
/// filling this field in.
fn profile_time_zone(site: &Site, published_at: time::OffsetDateTime) -> String {
    let seconds = site.time_zone.local(published_at).offset_seconds;
    let sign = if seconds < 0 { '-' } else { '+' };
    let magnitude = seconds.unsigned_abs();
    format!(
        "{sign}{:02}:{:02}",
        magnitude / 3600,
        (magnitude % 3600) / 60
    )
}

fn site_of(
    site: &Site,
    lang: &str,
    now: &str,
    published_at: time::OffsetDateTime,
    rate_for: &dyn Fn(&ChargingPoint) -> Option<Rate>,
) -> SiteOut {
    SiteOut {
        id: site.facility.id.clone(),
        version: site.facility.version.to_string(),
        last_updated: now.to_owned(),
        name: Multilingual::new(lang, &site.name),
        location_reference: LocationReference {
            area: AreaLocation {
                coordinates: Coordinates {
                    latitude: Exact(site.coordinates.latitude),
                    longitude: Exact(site.coordinates.longitude),
                },
                extension: LocationExtension {
                    facility_location: FacilityLocation {
                        time_zone: profile_time_zone(site, published_at),
                        address: Address {
                            postcode: site.address.postcode.clone(),
                            city: Multilingual::new(lang, &site.address.city),
                            country_code: site.address.country_code.clone(),
                            address_line: vec![
                                AddressLine {
                                    order: 0,
                                    kind: Enumerated::new("street"),
                                    text: Multilingual::new(lang, &site.address.street),
                                },
                                AddressLine {
                                    order: 1,
                                    kind: Enumerated::new("houseNumber"),
                                    text: Multilingual::new(lang, &site.address.house_number),
                                },
                            ],
                        },
                    },
                },
            },
        },
        stations: site
            .stations
            .iter()
            .map(|station| station_of(station, lang, now, rate_for))
            .collect(),
    }
}

fn station_of(
    station: &Station,
    lang: &str,
    now: &str,
    rate_for: &dyn Fn(&ChargingPoint) -> Option<Rate>,
) -> StationOut {
    StationOut {
        id: station.facility.id.clone(),
        version: station.facility.version.to_string(),
        last_updated: now.to_owned(),
        operator: OperatorOut {
            organisation: Organisation {
                name: Multilingual::new(
                    lang,
                    format!(
                        "{}*{}",
                        station.operator.country_code(),
                        station.operator.party_id()
                    ),
                ),
            },
        },
        authentication: station
            .points
            .iter()
            .flat_map(|point| super::authentication_methods(point.ad_hoc_payment))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(Enumerated::new)
            .collect(),
        // Derived, never stored: `numberOfRefillPoints` is `1..1` in the
        // profile, and a count that disagrees with the list beside it is a
        // document two readers read differently.
        number_of_refill_points: station.points.len(),
        total_maximum_power: watts(station.total_max_power_kw),
        refill_points: station
            .points
            .iter()
            .map(|point| RefillPointOut {
                point: point_of(point, lang, now, rate_for),
            })
            .collect(),
    }
}

fn point_of(
    point: &ChargingPoint,
    lang: &str,
    now: &str,
    rate_for: &dyn Fn(&ChargingPoint) -> Option<Rate>,
) -> ElectricChargingPoint {
    ElectricChargingPoint {
        id: point.facility.id.clone(),
        version: point.facility.version.to_string(),
        last_updated: now.to_owned(),
        external_identifier: vec![ExternalIdentifier {
            identifier: point.evse_id.to_string(),
            type_of_identifier: Extended::new("evseId"),
        }],
        delivery_unit: Enumerated::new("kWh"),
        current_type: Enumerated::new(match point.current_type {
            CurrentType::Ac => "ac",
            CurrentType::Dc => "dc",
        }),
        v2g: vec![Enumerated::new(super::v2g_literal(point.v2g))],
        number_of_connectors: point.connectors.len(),
        available_charging_power: vec![watts(point.max_power_kw)],
        connector: point.connectors.iter().map(connector_of).collect(),
        electric_energy: rate_for(point)
            .map(|rate| {
                vec![ElectricEnergy {
                    energy_rate: vec![rate_of(&rate, lang, now)],
                }]
            })
            .unwrap_or_default(),
    }
}

fn connector_of(connector: &Connector) -> ConnectorOut {
    ConnectorOut {
        connector_type: Enumerated::new(connector.kind.as_profile_str()),
        max_power_at_socket: watts(connector.max_power_kw),
    }
}

fn rate_of(rate: &Rate, lang: &str, now: &str) -> EnergyRate {
    EnergyRate {
        id: rate.id.clone(),
        rate_policy: Enumerated::new(rate.policy.as_profile_str()),
        last_updated: now.to_owned(),
        applicable_currency: vec![rate.currency.to_string()],
        rate_name: rate.name.as_ref().map(|name| Multilingual::new(lang, name)),
        combination_with_parking_fee: rate.combination_with_parking_fee,
        minimum_delivery_fee: rate.minimum_delivery_fee.map(Exact),
        maximum_delivery_fee: rate.maximum_delivery_fee.map(Exact),
        energy_price: rate.prices.iter().map(price_of).collect(),
    }
}

fn price_of(price: &Price) -> EnergyPrice {
    EnergyPrice {
        price_type: Enumerated::new(price.price_type.as_profile_str()),
        value: Exact(price.value),
        tax_included: price.tax_included,
        tax_rate: price.tax_rate.map(Exact),
        // `en`, not the publication's language: the sentence is generated by
        // this crate and is English, and a `MultilingualString` labelled `de`
        // carrying English is a document that lies about a field whose only
        // job is to say what language the text is in.
        additional_information: price
            .additional_information
            .as_ref()
            .map(|text| Multilingual::new("en", text)),
        overall_period: price.period.as_ref().map(period_of),
        // A tiered price published without the band it applies in reads as
        // unconditional, which is a feed that under-states its own prices.
        energy_based_applicability: price.energy_applicability.map(|band| {
            EnergyBasedApplicability {
                from_kwh: band.from_kwh,
                to_kwh: band.to_kwh,
            }
        }),
        time_based_applicability: price.time_applicability.map(|band| TimeBasedApplicability {
            from_minute: band.from_minute,
            to_minute: band.to_minute,
        }),
    }
}

fn period_of(period: &Period) -> OverallPeriod {
    OverallPeriod {
        start: period.from.map(timestamp),
        end: period.until.map(timestamp),
        valid_period: period
            .daily
            .map(|(start, end)| {
                vec![ValidPeriod {
                    recurring: vec![TimePeriodOfDay {
                        start: time_of_day(start),
                        end: time_of_day(end),
                    }],
                }]
            })
            .unwrap_or_default(),
    }
}
