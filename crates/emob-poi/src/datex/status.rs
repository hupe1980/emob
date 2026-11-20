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

use datex2::v3::afir_energy_infrastructure::{
    self as aegi, AnyRefillPointStatus, EnergyInfrastructureSiteStatus,
    EnergyInfrastructureStationStatus, EnergyInfrastructureStatusPublication,
};
use datex2::v3::common::{Reference, VersionedReference};
use datex2::v3::message_container::MessageContainer;
use datex2::v3::{Publication, json};

use super::table::{InformationStatus, Publisher, header};
use super::wire::{date_time, money};
use crate::rate::PriceType;
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
    /// The new price type.
    pub price_type: PriceType,
    /// The new amount.
    pub value: rust_decimal::Decimal,
}

/// The dynamic publication: a status publication in the message container the
/// Mobilithek expects, marked as a complete snapshot (`snapshotPush`) by
/// `datex2`'s own `snapshot`.
#[derive(Debug, Clone)]
pub struct StatusPublication {
    container: MessageContainer,
}

/// A versioned reference into the table publication. `datex2` writes the
/// `targetClass` the profile asks for beside it.
fn reference(facility: &Facility) -> VersionedReference {
    VersionedReference {
        id: Some(facility.id.clone()),
        version: Some(facility.version.to_string()),
    }
}

fn id_of(reference: Option<&VersionedReference>) -> Option<&str> {
    reference.and_then(|reference| reference.id.as_deref())
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
    let now = date_time(published_at);

    // Group without reordering: a status feed is read in order, and a producer
    // that sorts its own points makes every diff against the previous message
    // look like a change.
    let mut sites: Vec<EnergyInfrastructureSiteStatus> = Vec::new();
    for update in updates {
        let at_site = sites
            .iter()
            .position(|existing| id_of(existing.reference.as_deref()) == Some(&update.site.id))
            .unwrap_or_else(|| {
                sites.push(EnergyInfrastructureSiteStatus {
                    reference: Some(Box::new(reference(&update.site))),
                    last_updated: Some(now),
                    ..EnergyInfrastructureSiteStatus::default()
                });
                sites.len() - 1
            });
        let stations = &mut sites[at_site].energy_infrastructure_station_status;

        let at_station = stations
            .iter()
            .position(|existing| id_of(existing.reference.as_deref()) == Some(&update.station.id))
            .unwrap_or_else(|| {
                stations.push(EnergyInfrastructureStationStatus {
                    reference: Some(Box::new(reference(&update.station))),
                    last_updated: Some(now),
                    ..EnergyInfrastructureStationStatus::default()
                });
                stations.len() - 1
            });

        stations[at_station].refill_point_status.push(
            AnyRefillPointStatus::ElectricChargingPointStatus(Box::new(
                aegi::ElectricChargingPointStatus {
                    reference: Some(Box::new(reference(&update.point))),
                    last_updated: Some(now),
                    status: Some(super::point_status(update.report.status())),
                    energy_rate_update: update
                        .price
                        .iter()
                        .map(|price| aegi::EnergyRateUpdate {
                            last_updated: Some(now),
                            energy_rate_reference: Some(Box::new(Reference {
                                id: Some(price.rate_id.clone()),
                            })),
                            energy_price: vec![aegi::EnergyPrice {
                                price_type: Some(super::price_type(price.price_type)),
                                value: money(price.value),
                                ..aegi::EnergyPrice::default()
                            }],
                            ..aegi::EnergyRateUpdate::default()
                        })
                        .collect(),
                    ..aegi::ElectricChargingPointStatus::default()
                },
            )),
        );
    }

    let publication = EnergyInfrastructureStatusPublication {
        lang: Some(publisher.language.clone()),
        publication_time: Some(now),
        publication_creator: Some(Box::new(publisher.creator())),
        header_information: Some(Box::new(header(status))),
        table_reference: vec![reference(table)],
        energy_infrastructure_site_status: sites,
        ..EnergyInfrastructureStatusPublication::default()
    };
    StatusPublication {
        container: json::mobilithek::snapshot(
            Publication::AfirEnergyInfrastructureStatus(Box::new(publication)),
            now,
        ),
    }
}

impl StatusPublication {
    /// The publication as the JSON the national access point ingests.
    ///
    /// # Errors
    ///
    /// [`datex2::Error`], naming the member and the rule, for anything the
    /// profile's schemas would reject.
    pub fn to_json(&self) -> Result<String, datex2::Error> {
        json::mobilithek::to_string(&self.container)
    }
}
