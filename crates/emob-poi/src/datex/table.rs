//! `EnergyInfrastructureTablePublication` — the static half of the feed.
//!
//! What `[AFIR Art. 20(2)(a)–(b)]` calls static data: where the points are,
//! what they are, what they cost. It changes when somebody sends an engineer,
//! and the dynamic publication addresses it by `idG` and `versionG` — so
//! republishing a table with a bumped version and not telling the status feed
//! is how a working status feed stops being delivered. [`crate::feed`] is where
//! the two are checked against each other.

use std::collections::{BTreeMap, BTreeSet};

use datex2::v3::afir_energy_infrastructure::{
    self as aegi, AnyEnergyProduct, DeliveryUnitEnum, EnergyInfrastructureSite,
    EnergyInfrastructureStation, EnergyInfrastructureTable, EnergyInfrastructureTablePublication,
    RefillPoint,
};
use datex2::v3::afir_facilities::{self as afac, AfirFacilityLocation, TypeOfIdentifierEnum};
use datex2::v3::common::{
    self as com, ConfidentialityValueEnum, HeaderInformation, InformationStatusEnum,
    InternationalIdentifier,
};
use datex2::v3::location_extension::{Address, AddressLine, AddressLineTypeEnum};
use datex2::v3::location_referencing::{
    AreaLocation, LocationExtensionType, LocationReference, PointCoordinates,
};
use datex2::v3::{Publication, json};

use super::wire::{date_time, float, money, text, time_of_day, watts};
use crate::rate::{Period, Price, Rate};
use crate::site::{ChargingPoint, Connector, Site, Station};

/// The profile version this module writes `[DATEX-II-Profil]`.
pub const PROFILE_VERSION: &str = json::mobilithek::PROFILE_VERSION;

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
    const fn profile(self) -> InformationStatusEnum {
        match self {
            Self::Real => InformationStatusEnum::Real,
            Self::Test => InformationStatusEnum::Test,
        }
    }
}

impl Publisher {
    /// `publicationCreator`.
    pub(super) fn creator(&self) -> InternationalIdentifier {
        InternationalIdentifier {
            country: Some(self.country.clone()),
            national_identifier: Some(self.national_identifier.clone()),
            ..InternationalIdentifier::default()
        }
    }
}

/// `headerInformation`: unrestricted, and real or a test.
pub(super) fn header(status: InformationStatus) -> HeaderInformation {
    HeaderInformation {
        confidentiality: Some(ConfidentialityValueEnum::NoRestriction),
        information_status: Some(status.profile()),
        ..HeaderInformation::default()
    }
}

/// The static publication.
#[derive(Debug, Clone)]
pub struct TablePublication {
    publication: Publication,
    /// What this document says it published, read back off the document.
    ///
    /// Not written — it is the table's own contents, not a field of the
    /// profile. It is here because a status publication addresses a facility
    /// **at the version the table published it at**, and the only party that
    /// can answer *which* version that was is the document that carried it. A
    /// service holding its own second copy of the inventory answers a different
    /// question: what it runs now (D308).
    published: Published,
    /// The instant this document was written for.
    ///
    /// Carried rather than re-supplied by the caller: a service recording what
    /// the access point accepted was passing this in beside the publication,
    /// which is a fact the document already states and a caller could state
    /// differently.
    published_at: time::OffsetDateTime,
}

/// What one [`TablePublication`] put on the record.
///
/// Every facility it carried, at the version it carried it at, and every rate
/// it gave an `idG` to. This is what a dynamic update has to resolve against:
/// `[DATEX-II-Profil]` gives a consumer no way to ask about a version it never
/// received, so a status message referencing one is dropped in silence.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Published {
    /// Facility `idG` → the `versionG` this table published it at, as written.
    facilities: BTreeMap<String, String>,
    /// Every rate `idG` a price update may address.
    rates: BTreeSet<String>,
}

impl Published {
    /// The version this table published a facility at, if it carried it.
    #[must_use]
    pub fn version_of(&self, facility: &str) -> Option<&str> {
        self.facilities.get(facility).map(String::as_str)
    }

    /// Whether this table gave a rate that `idG`.
    #[must_use]
    pub fn carries_rate(&self, rate_id: &str) -> bool {
        self.rates.contains(rate_id)
    }

    /// Read back off the table this publication actually built.
    ///
    /// Off the built document rather than off the inventory it was built from,
    /// so there is no second walk to disagree with the first.
    fn of(table: &EnergyInfrastructureTable) -> Self {
        let mut published = Self::default();
        published.facility(table.id.as_ref(), table.version.as_ref());
        for site in &table.energy_infrastructure_site {
            published.facility(site.id.as_ref(), site.version.as_ref());
            for station in &site.energy_infrastructure_station {
                published.facility(station.id.as_ref(), station.version.as_ref());
                for refill in &station.refill_point {
                    let RefillPoint::ElectricChargingPoint(point) = refill else {
                        continue;
                    };
                    published.facility(point.id.as_ref(), point.version.as_ref());
                    for product in &point.energy_product {
                        let AnyEnergyProduct::ElectricEnergy(energy) = product else {
                            continue;
                        };
                        published
                            .rates
                            .extend(energy.energy_rate.iter().filter_map(|rate| rate.id.clone()));
                    }
                }
            }
        }
        published
    }

    fn facility(&mut self, id: Option<&String>, version: Option<&String>) {
        if let (Some(id), Some(version)) = (id, version) {
            self.facilities.insert(id.clone(), version.clone());
        }
    }
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
    let now = date_time(published_at);
    let built = EnergyInfrastructureTable {
        id: Some(table.id.clone()),
        version: Some(table.version.to_string()),
        table_name: table_name.map(ToOwned::to_owned),
        energy_infrastructure_site: sites
            .iter()
            .map(|site| site_of(site, &publisher.language, now, published_at, rate_for))
            .collect(),
        ..EnergyInfrastructureTable::default()
    };
    // Read off the table that was just built, not off the inventory it was
    // built from. See `Published`.
    let manifest = Published::of(&built);
    TablePublication {
        publication: Publication::AfirEnergyInfrastructureTable(Box::new(
            EnergyInfrastructureTablePublication {
                lang: Some(publisher.language.clone()),
                publication_time: Some(now),
                publication_creator: Some(Box::new(publisher.creator())),
                header_information: Some(Box::new(header(status))),
                energy_infrastructure_table: vec![built],
                ..EnergyInfrastructureTablePublication::default()
            },
        )),
        published: manifest,
        published_at,
    }
}

impl TablePublication {
    /// What this document put on the record, for a status publication to
    /// resolve its references against.
    #[must_use]
    pub const fn published(&self) -> &Published {
        &self.published
    }

    /// The instant this document was written for.
    #[must_use]
    pub const fn published_at(&self) -> time::OffsetDateTime {
        self.published_at
    }

    /// The publication as the JSON the national access point ingests.
    ///
    /// # Errors
    ///
    /// [`datex2::Error`], naming the member and the rule, for anything the
    /// profile's schemas would reject — an amount with a tenth of a cent in
    /// it, for one, which [`crate::feed::Feed::check`] refuses first.
    pub fn to_json(&self) -> Result<String, datex2::Error> {
        json::mobilithek::to_string(&self.publication)
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
    now: datex2::DateTime,
    published_at: time::OffsetDateTime,
    rate_for: &dyn Fn(&ChargingPoint) -> Option<Rate>,
) -> EnergyInfrastructureSite {
    let line = |order: u64, kind: AddressLineTypeEnum, value: &str| AddressLine {
        order: Some(order),
        r#type: Some(kind),
        text: Some(text(lang, value)),
        ..AddressLine::default()
    };
    let location = AreaLocation {
        coordinates_for_display: Some(Box::new(PointCoordinates {
            latitude: float(site.coordinates.latitude),
            longitude: float(site.coordinates.longitude),
            ..PointCoordinates::default()
        })),
        location_extension: Some(Box::new(LocationExtensionType {
            afir_facility_location: Some(Box::new(AfirFacilityLocation {
                time_zone: Some(profile_time_zone(site, published_at)),
                address: Some(Box::new(Address {
                    postcode: Some(site.address.postcode.clone()),
                    city: Some(text(lang, &site.address.city)),
                    country_code: Some(site.address.country_code.clone()),
                    address_line: vec![
                        line(0, AddressLineTypeEnum::Street, &site.address.street),
                        line(
                            1,
                            AddressLineTypeEnum::HouseNumber,
                            &site.address.house_number,
                        ),
                    ],
                    ..Address::default()
                })),
                ..AfirFacilityLocation::default()
            })),
            ..LocationExtensionType::default()
        })),
        ..AreaLocation::default()
    };
    EnergyInfrastructureSite {
        id: Some(site.facility.id.clone()),
        version: Some(site.facility.version.to_string()),
        last_updated: Some(now),
        name: Some(text(lang, &site.name)),
        location_reference: Some(LocationReference::AreaLocation(Box::new(location))),
        energy_infrastructure_station: site
            .stations
            .iter()
            .map(|station| station_of(station, lang, now, rate_for))
            .collect(),
        ..EnergyInfrastructureSite::default()
    }
}

fn station_of(
    station: &Station,
    lang: &str,
    now: datex2::DateTime,
    rate_for: &dyn Fn(&ChargingPoint) -> Option<Rate>,
) -> EnergyInfrastructureStation {
    let mut authentication: Vec<_> = Vec::new();
    for method in station
        .points
        .iter()
        .flat_map(|point| super::authentication_methods(point.ad_hoc_payment))
    {
        if !authentication.contains(&method) {
            authentication.push(method);
        }
    }
    authentication.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    EnergyInfrastructureStation {
        id: Some(station.facility.id.clone()),
        version: Some(station.facility.version.to_string()),
        last_updated: Some(now),
        operator: Some(afac::Organisation::AnOrganisation(Box::new(
            afac::AnOrganisation {
                name: Some(text(
                    lang,
                    format!(
                        "{}*{}",
                        station.operator.country_code(),
                        station.operator.party_id()
                    ),
                )),
                ..afac::AnOrganisation::default()
            },
        ))),
        authentication_and_identification_methods: authentication,
        // Derived, never stored: `numberOfRefillPoints` is `1..1` in the
        // profile, and a count that disagrees with the list beside it is a
        // document two readers read differently.
        number_of_refill_points: Some(station.points.len() as u64),
        service_type: vec![aegi::ServiceType {
            service_type: Some(if station.staffed {
                aegi::ServiceTypeEnum::PhysicalAttendance
            } else {
                aegi::ServiceTypeEnum::Unattended
            }),
            ..aegi::ServiceType::default()
        }],
        total_maximum_power: watts(station.total_max_power_kw),
        refill_point: station
            .points
            .iter()
            .map(|point| {
                RefillPoint::ElectricChargingPoint(Box::new(point_of(point, lang, now, rate_for)))
            })
            .collect(),
        ..EnergyInfrastructureStation::default()
    }
}

fn point_of(
    point: &ChargingPoint,
    lang: &str,
    now: datex2::DateTime,
    rate_for: &dyn Fn(&ChargingPoint) -> Option<Rate>,
) -> aegi::ElectricChargingPoint {
    aegi::ElectricChargingPoint {
        id: Some(point.facility.id.clone()),
        version: Some(point.facility.version.to_string()),
        last_updated: Some(now),
        // `typeOfIdentifier` has no `evseId` literal, so it is the profile's
        // extension value — written `extendedG` with `evseId` beside it.
        external_identifier: vec![afac::ExternalIdentifier {
            identifier: Some(point.evse_id.to_string()),
            type_of_identifier: Some(TypeOfIdentifierEnum::Extended("evseId".to_owned())),
            ..afac::ExternalIdentifier::default()
        }],
        delivery_unit: Some(DeliveryUnitEnum::KWh),
        current_type: Some(super::current_type(point.current_type)),
        vehicle_to_grid_communication_type: vec![super::v2g_type(point.v2g)],
        number_of_connectors: Some(point.connectors.len() as u64),
        available_charging_power: watts(point.max_power_kw).into_iter().collect(),
        connector: point.connectors.iter().map(connector_of).collect(),
        energy_product: rate_for(point)
            .map(|rate| {
                vec![AnyEnergyProduct::ElectricEnergy(Box::new(
                    aegi::ElectricEnergy {
                        energy_rate: vec![rate_of(&rate, lang, now)],
                        ..aegi::ElectricEnergy::default()
                    },
                ))]
            })
            .unwrap_or_default(),
        ..aegi::ElectricChargingPoint::default()
    }
}

fn connector_of(connector: &Connector) -> aegi::Connector {
    aegi::Connector {
        connector_type: Some(super::connector_type(connector.kind)),
        max_power_at_socket: watts(connector.max_power_kw),
        ..aegi::Connector::default()
    }
}

fn rate_of(rate: &Rate, lang: &str, now: datex2::DateTime) -> aegi::EnergyRate {
    aegi::EnergyRate {
        id: Some(rate.id.clone()),
        rate_policy: Some(super::rate_policy(rate.policy)),
        last_updated: Some(now),
        applicable_currency: vec![rate.currency.to_string()],
        rate_name: rate.name.as_ref().map(|name| text(lang, name)),
        combination_with_parking_fee: rate.combination_with_parking_fee,
        minimum_delivery_fee: rate.minimum_delivery_fee.and_then(money),
        maximum_delivery_fee: rate.maximum_delivery_fee.and_then(money),
        energy_price: rate.prices.iter().map(price_of).collect(),
        ..aegi::EnergyRate::default()
    }
}

pub(super) fn price_of(price: &Price) -> aegi::EnergyPrice {
    aegi::EnergyPrice {
        price_type: Some(super::price_type(price.price_type)),
        value: money(price.value),
        tax_included: price.tax_included,
        tax_rate: price.tax_rate.and_then(float),
        // `en`, not the publication's language: the sentence is generated by
        // this crate and is English, and a `MultilingualString` labelled `de`
        // carrying English is a document that lies about a field whose only
        // job is to say what language the text is in.
        additional_information: price
            .additional_information
            .as_ref()
            .map(|sentence| text("en", sentence)),
        overall_period: price
            .period
            .as_ref()
            .map(|period| Box::new(period_of(period))),
        // A tiered price published without the band it applies in reads as
        // unconditional, which is a feed that under-states its own prices.
        energy_based_applicability: price.energy_applicability.map(|band| {
            Box::new(aegi::EnergyBasedApplicability {
                from_k_wh: band.from_kwh.map(u64::from),
                to_k_wh: band.to_kwh.map(u64::from),
                ..aegi::EnergyBasedApplicability::default()
            })
        }),
        time_based_applicability: price.time_applicability.map(|band| {
            Box::new(aegi::TimeBasedApplicability {
                from_minute: band.from_minute.map(u64::from),
                to_minute: band.to_minute.map(u64::from),
                ..aegi::TimeBasedApplicability::default()
            })
        }),
        ..aegi::EnergyPrice::default()
    }
}

fn period_of(period: &Period) -> com::OverallPeriod {
    com::OverallPeriod {
        overall_start_time: period.from.map(date_time),
        overall_end_time: period.until.map(date_time),
        valid_period: period
            .daily
            .map(|(start, end)| {
                vec![com::Period {
                    recurring_time_period_of_day: vec![com::TimePeriodOfDay {
                        start_time_of_period: Some(time_of_day(start)),
                        end_time_of_period: Some(time_of_day(end)),
                        ..com::TimePeriodOfDay::default()
                    }],
                    ..com::Period::default()
                }]
            })
            .unwrap_or_default(),
        ..com::OverallPeriod::default()
    }
}
