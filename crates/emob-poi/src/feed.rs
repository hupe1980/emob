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
use crate::datex::table::{InformationStatus, Publisher};
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
    /// Whatever [`crate::site::Station::check`] objects to, and
    /// [`PoiError::RateZoneIsNotTheSites`] for a price published on the wrong
    /// clock.
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
    /// # Errors
    ///
    /// [`PoiError::FacilityNotPublished`] or [`PoiError::VersionNotPublished`]
    /// when an update addresses something this feed's table does not carry at
    /// that version — the failure that is otherwise silent.
    pub fn status(
        &self,
        updates: &[PointUpdate],
        published_at: time::OffsetDateTime,
    ) -> Result<StatusPublication> {
        check(&self.sites, updates)?;
        Ok(status::publication(
            &self.publisher,
            self.information_status,
            &self.table,
            updates,
            published_at,
        ))
    }
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
        // Through the published point, because that is the only constructor —
        // and the one the deleted `Report::operating` used to let every test
        // walk around (D217).
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
}
