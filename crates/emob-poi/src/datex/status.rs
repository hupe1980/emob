//! `EnergyInfrastructureStatusPublication` — the dynamic half of the feed.
//!
//! `[AFIR Art. 20(2)(c)]` calls it dynamic data: operational status,
//! availability, and the ad-hoc price. It carries no infrastructure of its own.
//! Every object it speaks about is a **reference** into the table publication —
//! `targetClass`, `idG` and `versionG` — which has two consequences worth
//! stating plainly:
//!
//! 1. A status for an object the table never published cannot be delivered, and
//!    a consumer's only sane response is to drop it. Silently.
//! 2. The same is true of a status for a *version* the table never published.
//!    So bumping a point's version in the table and forgetting to bump it here
//!    takes that point's availability off the map without any error anywhere.
//!
//! Neither failure appears in a schema validation, in an HTTP status code or in
//! a log. [`crate::feed`] is where they are made to appear.

#![allow(
    clippy::struct_field_names,
    reason = "the fields are named for the profile's own attributes, and a Rust \
              name that differs from the JSON key it writes is the one mistake \
              these structs exist to prevent"
)]

use serde::Serialize;

use super::table::{InformationStatus, PROFILE_VERSION, Publisher};
use super::wire::{Enumerated, Exact, timestamp};
use crate::site::Facility;
use crate::status::Report;

/// One point's live state, and its price if that changed too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointUpdate {
    /// Which site the point is on.
    pub site: Facility,
    /// Which station it belongs to.
    pub station: Facility,
    /// The point itself.
    pub point: Facility,
    /// What it is doing, checked against the register.
    pub report: Report,
    /// A price change, when there is one.
    ///
    /// `[AFIR Art. 20(2)(c)]` makes the ad-hoc price dynamic, so a tariff that
    /// changes at noon is a status message and not a republished table. The
    /// rate is addressed by the `idG` the table gave it.
    pub price: Option<PriceUpdate>,
}

/// A price change, addressed to a rate the table already published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceUpdate {
    /// The `idG` of the rate in the table publication.
    pub rate_id: String,
    /// The new price type, in the profile's spelling.
    pub price_type: &'static str,
    /// The new amount.
    pub value: rust_decimal::Decimal,
}

/// The dynamic publication.
#[derive(Debug, Clone, Serialize)]
pub struct StatusPublication {
    #[serde(rename = "messageContainer")]
    message_container: MessageContainer,
}

#[derive(Debug, Clone, Serialize)]
struct MessageContainer {
    payload: Vec<Payload>,
    #[serde(rename = "exchangeInformation")]
    exchange_information: ExchangeInformation,
}

#[derive(Debug, Clone, Serialize)]
struct Payload {
    #[serde(rename = "modelBaseVersionG")]
    model_base_version: &'static str,
    #[serde(rename = "profileNameG")]
    profile_name: &'static str,
    #[serde(rename = "profileVersionG")]
    profile_version: &'static str,
    #[serde(rename = "aegiEnergyInfrastructureStatusPublication")]
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
    #[serde(rename = "tableReference")]
    table_reference: Vec<Reference>,
    #[serde(rename = "energyInfrastructureSiteStatus")]
    sites: Vec<SiteStatus>,
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

/// A versioned reference into the table publication.
#[derive(Debug, Clone, Serialize)]
struct Reference {
    #[serde(rename = "targetClass")]
    target_class: &'static str,
    #[serde(rename = "idG")]
    id: String,
    #[serde(rename = "versionG")]
    version: String,
}

impl Reference {
    fn facility(facility: &Facility) -> Self {
        Self {
            target_class: "FacilityObject",
            id: facility.id.clone(),
            version: facility.version.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct SiteStatus {
    reference: Reference,
    #[serde(rename = "lastUpdated")]
    last_updated: String,
    #[serde(rename = "energyInfrastructureStationStatus")]
    stations: Vec<StationStatus>,
}

#[derive(Debug, Clone, Serialize)]
struct StationStatus {
    reference: Reference,
    #[serde(rename = "lastUpdated")]
    last_updated: String,
    #[serde(rename = "refillPointStatus")]
    points: Vec<RefillPointStatus>,
}

#[derive(Debug, Clone, Serialize)]
struct RefillPointStatus {
    #[serde(rename = "aegiElectricChargingPointStatus")]
    point: ElectricChargingPointStatus,
}

#[derive(Debug, Clone, Serialize)]
struct ElectricChargingPointStatus {
    reference: Reference,
    #[serde(rename = "lastUpdated")]
    last_updated: String,
    status: Enumerated,
    #[serde(rename = "energyRateUpdate", skip_serializing_if = "Vec::is_empty")]
    energy_rate_update: Vec<EnergyRateUpdate>,
}

#[derive(Debug, Clone, Serialize)]
struct EnergyRateUpdate {
    #[serde(rename = "lastUpdated")]
    last_updated: String,
    #[serde(rename = "energyRateReference")]
    energy_rate_reference: RateReference,
    #[serde(rename = "energyPrice")]
    energy_price: Vec<PriceOut>,
}

#[derive(Debug, Clone, Serialize)]
struct RateReference {
    #[serde(rename = "targetClass")]
    target_class: &'static str,
    #[serde(rename = "idG")]
    id: String,
}

#[derive(Debug, Clone, Serialize)]
struct PriceOut {
    #[serde(rename = "priceType")]
    price_type: Enumerated,
    value: Exact,
}

#[derive(Debug, Clone, Serialize)]
struct ExchangeInformation {
    #[serde(rename = "exchangeContext")]
    exchange_context: ExchangeContext,
    #[serde(rename = "dynamicInformation")]
    dynamic_information: DynamicInformation,
}

#[derive(Debug, Clone, Serialize)]
struct ExchangeContext {
    #[serde(rename = "codedExchangeProtocol")]
    coded_exchange_protocol: Enumerated,
    #[serde(rename = "exchangeSpecificationVersion")]
    exchange_specification_version: &'static str,
    #[serde(rename = "supplierOrCisRequester")]
    supplier: Empty,
}

#[derive(Debug, Clone, Serialize)]
struct Empty {}

#[derive(Debug, Clone, Serialize)]
struct DynamicInformation {
    #[serde(rename = "exchangeStatus")]
    exchange_status: Enumerated,
    #[serde(rename = "messageGenerationTimestamp")]
    message_generation_timestamp: String,
}

/// Build the dynamic publication for a set of point updates.
///
/// The updates are grouped back into the site → station → point nesting the
/// profile requires, in the order they arrive. `table` is the table publication
/// these references point into; getting it wrong is the failure this module's
/// documentation opens with, and [`crate::feed::check`] is what catches it.
#[must_use]
pub fn publication(
    publisher: &Publisher,
    status: InformationStatus,
    table: &Facility,
    updates: &[PointUpdate],
    published_at: time::OffsetDateTime,
) -> StatusPublication {
    let now = timestamp(published_at);

    // Group without reordering: a status feed is read in order, and a producer
    // that sorts its own points makes every diff against the previous message
    // look like a change.
    let mut sites: Vec<SiteStatus> = Vec::new();
    for update in updates {
        let at_site = sites
            .iter()
            .position(|existing| existing.reference.id == update.site.id)
            .unwrap_or_else(|| {
                sites.push(SiteStatus {
                    reference: Reference::facility(&update.site),
                    last_updated: now.clone(),
                    stations: Vec::new(),
                });
                sites.len() - 1
            });
        let stations = &mut sites[at_site].stations;

        let at_station = stations
            .iter()
            .position(|existing| existing.reference.id == update.station.id)
            .unwrap_or_else(|| {
                stations.push(StationStatus {
                    reference: Reference::facility(&update.station),
                    last_updated: now.clone(),
                    points: Vec::new(),
                });
                stations.len() - 1
            });

        stations[at_station].points.push(RefillPointStatus {
            point: ElectricChargingPointStatus {
                reference: Reference::facility(&update.point),
                last_updated: now.clone(),
                status: Enumerated::new(update.report.status().as_profile_str()),
                energy_rate_update: update
                    .price
                    .iter()
                    .map(|price| EnergyRateUpdate {
                        last_updated: now.clone(),
                        energy_rate_reference: RateReference {
                            target_class: "EnergyRate",
                            id: price.rate_id.clone(),
                        },
                        energy_price: vec![PriceOut {
                            price_type: Enumerated::new(price.price_type),
                            value: Exact(price.value),
                        }],
                    })
                    .collect(),
            },
        });
    }

    StatusPublication {
        message_container: MessageContainer {
            payload: vec![Payload {
                model_base_version: "3",
                profile_name: "AFIR Energy Infrastructure",
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
                    table_reference: vec![Reference {
                        target_class: "EnergyInfrastructureTable",
                        id: table.id.clone(),
                        version: table.version.to_string(),
                    }],
                    sites,
                },
            }],
            exchange_information: ExchangeInformation {
                exchange_context: ExchangeContext {
                    coded_exchange_protocol: Enumerated::new("snapshotPush"),
                    exchange_specification_version: "3.0",
                    supplier: Empty {},
                },
                dynamic_information: DynamicInformation {
                    exchange_status: Enumerated::new("online"),
                    message_generation_timestamp: now,
                },
            },
        },
    }
}

impl StatusPublication {
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
