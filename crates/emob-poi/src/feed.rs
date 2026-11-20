//! The two publications, held together.
//!
//! # The failure this module exists for
//!
//! A DATEX II status message says nothing on its own. Every object in it is a
//! reference — `targetClass`, `idG`, `versionG` — into a table publication that
//! was sent separately, on a different schedule, usually by a different job. A
//! reference that does not resolve is not an error: the consumer has nothing to
//! attach the status to, so it drops it, and the point's availability
//! disappears from every map that reads the feed. No HTTP status changes. No
//! schema validation fails. Nobody is told.
//!
//! It is the quietest failure in `[AFIR Art. 20]` compliance and it has three
//! ordinary causes: publishing a status for a point the table does not carry,
//! bumping a facility's `versionG` in the table without telling the status job,
//! and bumping it in the status job without republishing the table.
//!
//! [`Feed`] makes all three unreachable by building both publications from one
//! inventory. [`check`] is for a deployment that cannot do that — one where the
//! table is exported nightly by a different system — and wants the disagreement
//! to be an error rather than a silence.

use crate::datex::status::PointUpdate;
use crate::datex::table::{InformationStatus, Published, Publisher};
use crate::datex::{StatusPublication, TablePublication, status, table};
use crate::error::{PoiError, Result};
use crate::rate::Rate;
use crate::site::{ChargingPoint, Facility, Site};

/// One operator's published infrastructure, and the feeds it generates.
///
/// Both publications come from this one value, so the references in the status
/// message address facilities at the versions the table publishes them at —
/// there is no second inventory to drift from.
pub struct Feed<'a> {
    /// Who publishes, and under what national identifier.
    pub publisher: Publisher,
    /// Whether this describes reality.
    pub information_status: InformationStatus,
    /// The table's own identity, which the status publication references.
    pub table: Facility,
    /// A name for the table, when the operator groups its estate.
    pub table_name: Option<String>,
    /// The infrastructure.
    pub sites: Vec<Site>,
    /// The published rate for a point, when it has one.
    ///
    /// A closure rather than a field on the point: prices change on a different
    /// clock from concrete, and the same point may be published under different
    /// rates to different national access points.
    pub rate_for: &'a dyn Fn(&ChargingPoint) -> Option<Rate>,
}

impl Feed<'_> {
    /// Every station's power claim checked against the points it holds, and
    /// every published price checked against the clock the site runs on.
    ///
    /// # The second half is the one that is otherwise silent
    ///
    /// A tariff's `22:00` is local civil time at the charge point, and the site
    /// says which clock that is. Publishing a rate written in `Europe/Berlin` at
    /// a site in `Europe/Lisbon` produces a feed whose night price starts an
    /// hour after the driver standing there thinks it does — a well-formed
    /// document, a lawful tariff, a real site, and nothing failing until
    /// somebody compares a bill against a map. It is exactly the display-versus-
    /// bill drift `emob_tariff::display` exists to prevent, one object out.
    ///
    /// # Errors
    ///
    /// Whatever [`crate::site::Station::check`] objects to,
    /// [`PoiError::RateZoneIsNotTheSites`] for a price published on the wrong
    /// clock, and [`PoiError::AmountNotInCents`] for a price the profile cannot
    /// state.
    pub fn check(&self) -> Result<()> {
        self.sites.iter().try_for_each(Site::check)?;
        for site in &self.sites {
            for point in site.points() {
                let Some(rate) = (self.rate_for)(point) else {
                    continue;
                };
                if rate.time_zone != site.time_zone.name() {
                    return Err(PoiError::RateZoneIsNotTheSites {
                        rate: rate.id,
                        rate_zone: rate.time_zone,
                        site: site.facility.id.clone(),
                        site_zone: site.time_zone.to_string(),
                    });
                }
                in_cents(&rate)?;
            }
        }
        Ok(())
    }

    /// The static publication `[AFIR Art. 20(2)(a)–(b)]`.
    ///
    /// # Errors
    ///
    /// As [`Feed::check`] — a feed is not published before its own
    /// contradictions are.
    pub fn table(&self, published_at: time::OffsetDateTime) -> Result<TablePublication> {
        self.check()?;
        Ok(table::publication(
            &self.publisher,
            self.information_status,
            &self.table,
            self.table_name.as_deref(),
            &self.sites,
            published_at,
            self.rate_for,
        ))
    }

    /// The dynamic publication `[AFIR Art. 20(2)(c)]`.
    ///
    /// # Two inventories, and the update has to resolve against both
    ///
    /// `accepted` is what the national access point actually **has** — the
    /// [`Published`] manifest of the table publication it took — and it is a
    /// different object from `self.sites`, which is what the operator runs
    /// now. An update is refused unless it resolves against both, and the two
    /// answer different questions:
    ///
    /// * the inventory says whether this describes something the operator runs,
    ///   at the version it holds, in a state `[LSV26 §5(3)]`'s register allows
    ///   it to be reported in;
    /// * the accepted table says whether the consumer on the other end has an
    ///   object to attach the message to at all.
    ///
    /// They agree until the estate moves, and between snapshots the estate
    /// always has. Checking only the first published a well-formed document
    /// addressed to versions nobody had, which `[DATEX-II-Profil]` gives a
    /// consumer no way to resolve and no way to complain about (D308).
    ///
    /// # Errors
    ///
    /// [`PoiError::FacilityNotPublished`] or [`PoiError::VersionNotPublished`]
    /// when an update addresses something this feed's own inventory does not
    /// carry at that version; [`PoiError::NotInAcceptedTable`] or
    /// [`PoiError::RateNotInAcceptedTable`] when it addresses something the
    /// accepted table does not — the failure that is otherwise silent; and
    /// [`PoiError::AmountNotInCents`] for a new price the profile cannot state.
    pub fn status(
        &self,
        accepted: &Published,
        updates: &[PointUpdate],
        published_at: time::OffsetDateTime,
    ) -> Result<StatusPublication> {
        check(&self.sites, updates)?;
        resolves(accepted, updates)?;
        for price in updates.iter().filter_map(|update| update.price.as_ref()) {
            cents(&price.rate_id, "energyPrice", price.value)?;
        }
        Ok(status::publication(
            &self.publisher,
            self.information_status,
            &self.table,
            updates,
            published_at,
        ))
    }
}

/// Every amount on a rate, in whole cents.
fn in_cents(rate: &Rate) -> Result<()> {
    for price in &rate.prices {
        cents(&rate.id, "energyPrice", price.value)?;
    }
    if let Some(fee) = rate.minimum_delivery_fee {
        cents(&rate.id, "minimumDeliveryFee", fee)?;
    }
    if let Some(fee) = rate.maximum_delivery_fee {
        cents(&rate.id, "maximumDeliveryFee", fee)?;
    }
    Ok(())
}

fn cents(rate: &str, field: &'static str, amount: rust_decimal::Decimal) -> Result<()> {
    if crate::datex::wire::in_cents(amount) {
        return Ok(());
    }
    Err(PoiError::AmountNotInCents {
        rate: rate.to_owned(),
        field,
        amount,
    })
}

/// Check that every status update resolves against the table the access point
/// **accepted**.
///
/// The mirror of [`check`], one party out: that one asks the operator's own
/// inventory whether the update describes something real, this asks the
/// consumer's copy whether it describes something it holds.
///
/// # Errors
///
/// [`PoiError::NotInAcceptedTable`] for a facility reference the accepted table
/// cannot resolve, and [`PoiError::RateNotInAcceptedTable`] for a price update
/// addressed to a rate it never named.
pub fn resolves(accepted: &Published, updates: &[PointUpdate]) -> Result<()> {
    for update in updates {
        for facility in [&update.site, &update.station, &update.point] {
            let published = accepted.version_of(&facility.id);
            if published != Some(facility.version.to_string().as_str()) {
                return Err(PoiError::NotInAcceptedTable {
                    facility: facility.id.clone(),
                    referenced: facility.version.to_string(),
                    accepted: published.map(ToOwned::to_owned),
                });
            }
        }
        // A price change is addressed to a rate the table already named, by the
        // `idG` it gave out. Nothing else in this crate checks that name, and
        // an unresolvable one is the same silence as an unresolvable version.
        if let Some(price) = &update.price
            && !accepted.carries_rate(&price.rate_id)
        {
            return Err(PoiError::RateNotInAcceptedTable {
                rate_id: price.rate_id.clone(),
            });
        }
    }
    Ok(())
}

/// Check that every status update resolves against a published inventory.
///
/// # Errors
///
/// [`PoiError::FacilityNotPublished`] when a reference names something absent,
/// and [`PoiError::VersionNotPublished`] when it names the right object at the
/// wrong version. The second is the one worth having a type for: it is
/// indistinguishable from a working feed until somebody notices a charger
/// missing from a map.
pub fn check(sites: &[Site], updates: &[PointUpdate]) -> Result<()> {
    for update in updates {
        let site = sites
            .iter()
            .find(|site| site.facility.id == update.site.id)
            .ok_or_else(|| PoiError::FacilityNotPublished {
                facility: update.site.id.clone(),
            })?;
        agree(&site.facility, &update.site)?;

        let station = site
            .stations
            .iter()
            .find(|station| station.facility.id == update.station.id)
            .ok_or_else(|| PoiError::FacilityNotPublished {
                facility: update.station.id.clone(),
            })?;
        agree(&station.facility, &update.station)?;

        let point = station
            .points
            .iter()
            .find(|point| point.facility.id == update.point.id)
            .ok_or_else(|| PoiError::FacilityNotPublished {
                facility: update.point.id.clone(),
            })?;
        agree(&point.facility, &update.point)?;

        // …and the register has the last word on what may be said about it.
        // `ChargingPoint::report` enforces this at construction against the
        // point the caller held; this asks the same question of the point this
        // feed actually publishes, which catches an update built against one
        // inventory and sent against another.
        point.report(update.report.status())?;
    }
    Ok(())
}

/// Two references to the same object must agree about its version.
fn agree(published: &Facility, referenced: &Facility) -> Result<()> {
    if published.version == referenced.version {
        return Ok(());
    }
    Err(PoiError::VersionNotPublished {
        facility: published.id.clone(),
        referenced: referenced.version.to_string(),
        published: published.version.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::{Address, ChargingPoint, Connector, ConnectorType, Coordinates, Station};
    use crate::status::{Lifecycle, PointStatus};
    use emob_core::{EvseId, PartyId};
    use rust_decimal::Decimal;

    fn inventory() -> Vec<Site> {
        let point = ChargingPoint::new(
            Facility::new("point"),
            EvseId::parse("DE*ABC*E00001").unwrap(),
            Connector::new(ConnectorType::Iec62196T2Combo, Decimal::from(150)),
        );
        vec![Site::new(
            Facility::new("site"),
            "Musterstadt Nord",
            Coordinates {
                latitude: Decimal::from_str_exact("50.779599").unwrap(),
                longitude: Decimal::from_str_exact("6.104507").unwrap(),
            },
            Address::default(),
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![Station::new(
                Facility::new("station"),
                PartyId::new("DE", "ABC").unwrap(),
                vec![point],
            )],
        )]
    }

    fn update(point: Facility) -> PointUpdate {
        // Through the published point, because that is the only constructor:
        // a status a caller can state beside a point is one every test walks
        // around (D217).
        let sites = inventory();
        let published = &sites[0].stations[0].points[0];
        PointUpdate {
            site: Facility::new("site"),
            station: Facility::new("station"),
            point,
            report: published
                .report(PointStatus::Available)
                .expect("an operating point may report `available`"),
            price: None,
        }
    }

    #[test]
    fn a_status_for_a_version_the_table_never_published_is_an_error_and_not_a_silence() {
        // The table publishes version 1. The status job, having been restarted
        // after somebody edited a connector, addresses version 2. Every
        // consumer drops it, and the charger vanishes from the map.
        let refused = check(
            &inventory(),
            &[update(Facility::new("point").at_version(2))],
        );
        assert!(matches!(refused, Err(PoiError::VersionNotPublished { .. })));
    }

    #[test]
    fn a_status_for_a_point_the_table_does_not_carry_is_an_error() {
        let refused = check(&inventory(), &[update(Facility::new("some-other-point"))]);
        assert!(matches!(
            refused,
            Err(PoiError::FacilityNotPublished { .. })
        ));
    }

    #[test]
    fn a_matching_reference_resolves() {
        assert!(check(&inventory(), &[update(Facility::new("point"))]).is_ok());
    }

    #[test]
    fn an_update_built_against_a_stale_inventory_still_cannot_contradict_the_register() {
        // The update was built when the point was operating. By the time it is
        // published the register says the point is gone — and the feed says
        // `available`.
        let mut sites = inventory();
        sites[0].stations[0].points[0].lifecycle = Lifecycle::Decommissioned;

        let refused = check(&sites, &[update(Facility::new("point"))]);
        assert!(matches!(
            refused,
            Err(PoiError::StatusContradictsRegister { .. })
        ));
    }

    #[test]
    fn a_price_published_on_a_clock_the_site_does_not_run_on_is_refused() {
        use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};

        let lisbon = Tariff::simple(
            "t".parse().unwrap(),
            emob_core::Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Lisbon").unwrap(),
            vec![PriceComponent::new(
                Dimension::Energy,
                Decimal::from_str_exact("0.49").unwrap(),
            )],
        );
        let (rate, _) = crate::rate::publish(&lisbon, "r-1");
        let sites = inventory(); // Europe/Berlin

        let feed = Feed {
            publisher: Publisher {
                country: "de".to_owned(),
                national_identifier: "DE-NAP".to_owned(),
                language: "de".to_owned(),
            },
            information_status: InformationStatus::Real,
            table: Facility::new("table"),
            table_name: None,
            sites,
            rate_for: &|_| Some(rate.clone()),
        };

        let refused = feed.check().unwrap_err();
        assert!(
            matches!(refused, PoiError::RateZoneIsNotTheSites { .. }),
            "{refused}"
        );
        assert!(refused.to_string().contains("Europe/Lisbon"));
        assert!(refused.to_string().contains("Europe/Berlin"));
    }

    #[test]
    fn a_price_with_a_fraction_of_a_cent_is_not_published_rounded() {
        use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};

        // `0.589` a kWh is a lawful tariff. The profile's `AmountOfMoney` has
        // two places after the point, so the feed can state `0.58` or `0.59`
        // and the operator charges neither.
        let tariff = Tariff::simple(
            "t".parse().unwrap(),
            emob_core::Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(
                Dimension::Energy,
                Decimal::from_str_exact("0.589").unwrap(),
            )],
        );
        let (rate, _) = crate::rate::publish(&tariff, "r-1");
        let feed = Feed {
            publisher: Publisher {
                country: "DE".to_owned(),
                national_identifier: "DE-NAP".to_owned(),
                language: "de".to_owned(),
            },
            information_status: InformationStatus::Real,
            table: Facility::new("table"),
            table_name: None,
            sites: inventory(),
            rate_for: &|_| Some(rate.clone()),
        };

        let refused = feed
            .table(time::macros::datetime!(2026-04-14 12:00 UTC))
            .unwrap_err();
        assert!(
            matches!(
                &refused,
                PoiError::AmountNotInCents { rate, field: "energyPrice", .. } if rate == "r-1"
            ),
            "{refused}"
        );
        assert!(refused.to_string().contains("0.589"), "{refused}");
    }
}
