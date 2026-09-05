//! The infrastructure a register holds: sites, stations, points, connectors.
//!
//! The shape is the DATEX II profile's, because that is the shape the duty is
//! written in. `[DATEX-II-Profil]` nests an `EnergyInfrastructureSite` — a place
//! a driver navigates to — around `EnergyInfrastructureStation`s, each of which
//! holds `RefillPoint`s, each of which offers `Connector`s. Four levels for what
//! a price list calls "a charger" is not over-modelling: the site is what a map
//! pin is, the station is what a grid connection and an `totalMaximumPower` are,
//! the point is what an EVSE id and a session are, and the connector is what a
//! cable fits.
//!
//! Everything here is *static* data in the sense of `[AFIR Art. 20(2)(a)–(b)]` —
//! facts that change when somebody sends an engineer, not when somebody plugs
//! in. What changes on a minute's notice is [`crate::status`].

pub use emob_core::ConnectorType;

use emob_core::{AdHocPayment, CurrentType, EvseId, PartyId, TimeZone, V2gCommunication};
use rust_decimal::Decimal;

use crate::error::{PoiError, Result};
use crate::status::Lifecycle;

/// The identity of a published object, as the profile addresses it.
///
/// Every facility in a DATEX II publication carries an `idG` and a `versionG`,
/// and the **status** publication addresses objects by both `[DATEX-II-Profil]`.
/// So a version is not decoration: it is half of the address a dynamic update is
/// delivered to, and bumping one without republishing the table is how a feed
/// goes dark. [`crate::feed`] is where that is checked.
///
/// The profile types `versionG` as a string and every published example writes a
/// decimal integer in it. It is a `u32` here because the one thing a version has
/// to do is order, and `"10" < "9"` is true of strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Facility {
    /// The stable identity. The profile's examples use UUIDs; anything stable
    /// and unique within the publication is admissible.
    pub id: String,
    /// Which revision of this object the table published.
    pub version: u32,
}

impl Facility {
    /// A facility at version 1.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            version: 1,
        }
    }

    /// The same object, one revision on.
    #[must_use]
    pub fn at_version(self, version: u32) -> Self {
        Self { version, ..self }
    }
}

/// A point on the earth, in WGS 84.
///
/// Exact decimal rather than binary floating point, for the reason everything
/// else in this workspace is: the profile transports coordinates as JSON
/// numbers, and a value that arrives as `50.779599` should leave as
/// `50.779599`. Six decimals is about 0.1 m at this latitude, which is the
/// difference between two bays of the same car park.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coordinates {
    /// Degrees north.
    pub latitude: Decimal,
    /// Degrees east.
    pub longitude: Decimal,
}

/// A postal address, in the parts the profile's `addressLine` list wants.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Address {
    /// The street, without the number.
    pub street: String,
    /// The house number, as written on the building.
    pub house_number: String,
    /// The postcode.
    pub postcode: String,
    /// The city.
    pub city: String,
    /// ISO 3166-1 alpha-2.
    pub country_code: String,
}

/// One socket or tethered cable, and what it can deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connector {
    /// Which interface.
    pub kind: ConnectorType,
    /// The most this socket can deliver, in kW.
    ///
    /// The profile carries `maxPowerAtSocket` in **watts**; kW is what a
    /// datasheet, a tariff and `[AFIR Art. 5(4)]`'s 50 kW threshold are written
    /// in, so the conversion happens once, at the wire.
    pub max_power_kw: Decimal,
}

impl Connector {
    /// A connector of a kind, rated in kW.
    #[must_use]
    pub const fn new(kind: ConnectorType, max_power_kw: Decimal) -> Self {
        Self { kind, max_power_kw }
    }
}

/// One charging point: what has an EVSE id, and what a session happens at.
///
/// A point may offer several connectors and can serve **one** vehicle at a
/// time — which is what makes it the unit AFIR counts, prices and reports
/// availability for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChargingPoint {
    /// How the publication addresses it.
    pub facility: Facility,
    /// The EVSE id, published as an `externalIdentifier` of type `evseId`.
    pub evse_id: EvseId,
    /// AC or DC.
    pub current_type: CurrentType,
    /// The most the point can deliver, in kW.
    pub max_power_kw: Decimal,
    /// The interfaces it offers.
    pub connectors: Vec<Connector>,
    /// How a driver with no contract pays here `[AFIR Art. 5(1)]`.
    pub ad_hoc_payment: AdHocPayment,
    /// Which vehicle-communication generations it speaks.
    pub v2g: V2gCommunication,
    /// Where it is in the `[LSV26 §4]` register.
    pub lifecycle: Lifecycle,
}

impl ChargingPoint {
    /// A point with one connector, operating, taking cards.
    ///
    /// The defaults are the compliant ones on purpose: `[AFIR Art. 5(1)]`
    /// requires ad-hoc payment at every publicly accessible point, and a
    /// constructor whose easy path is the non-compliant one is a constructor
    /// that produces non-compliant fleets.
    #[must_use]
    pub fn new(facility: Facility, evse_id: EvseId, connector: Connector) -> Self {
        Self {
            facility,
            evse_id,
            current_type: if connector.kind.is_dc() {
                CurrentType::Dc
            } else {
                CurrentType::Ac
            },
            max_power_kw: connector.max_power_kw,
            connectors: vec![connector],
            ad_hoc_payment: AdHocPayment::CardReader,
            v2g: V2gCommunication::pwm_only(),
            lifecycle: Lifecycle::Operating,
        }
    }

    /// The status this point may publish, checked against **its own** register
    /// entry.
    ///
    /// The only way to build a [`Report`](crate::status::Report), and it lives
    /// here rather than on that type because the lifecycle is a fact about a
    /// point (D217).
    ///
    /// # Errors
    ///
    /// [`PoiError::StatusContradictsRegister`] for a status the register
    /// forbids: a decommissioned point published as `available` is a driver
    /// sent to a concrete pad.
    pub fn report(&self, status: crate::status::PointStatus) -> Result<crate::status::Report> {
        crate::status::Report::checked(&self.facility.id, self.lifecycle, status)
    }

    /// The compliance profile of this point, as far as the **inventory** knows
    /// it.
    ///
    /// # The seam this closes
    ///
    /// [`emob_core::ChargePointProfile`]'s own documentation has always said
    /// that "`emob-poi` builds it from the OCPI model", and no such function
    /// existed: every field of every profile the calendar judged was typed in
    /// by a caller. That is the workspace's third rule — *a check fed by a
    /// caller is not a check* — at the largest scale it occurs in. A point
    /// whose connectors are `[AFIR Anh. II 1.1]`'s subject is published to the
    /// national access point out of `self.connectors`, and was judged out of
    /// whatever a compliance report happened to be handed.
    ///
    /// So the facts the inventory holds come from the inventory: the
    /// identifier, the current, the power, **the interfaces**, how a driver
    /// with no contract pays, which vehicle-communication generations the point
    /// speaks, and whether the register says it is live.
    ///
    /// # What it cannot answer, and does not pretend to
    ///
    /// Everything else is left at [`ChargePointProfile::bare`](emob_core::ChargePointProfile::bare)'s value for the
    /// caller to state — the notice dates, the metering posture, the price
    /// indication, the ownership arrangement. Those live in a register export,
    /// a type approval and a contract, and a bridge that guessed them would put
    /// the fault this function exists to remove one layer further in.
    ///
    /// `commissioned_on` is an argument for the same reason: a location model
    /// says a point is operating, not since when, and `[LSV26 §4(1) Nr. 1]`'s
    /// deadline runs from a date.
    ///
    /// ```
    /// use emob_poi::{ChargingPoint, Connector, ConnectorType, Facility};
    /// use emob_core::Accessibility;
    /// use emob_core::obligation::{ObligationId, Status, assess};
    /// use rust_decimal::Decimal;
    /// use time::macros::date;
    ///
    /// let point = ChargingPoint::new(
    ///     Facility::new("DE*ABC*P1"),
    ///     "DE*ABC*E123*1".parse()?,
    ///     Connector::new(ConnectorType::Chademo, Decimal::from(50)),
    /// );
    /// let profile = point.profile(date!(2025 - 01 - 01), Accessibility::Public);
    ///
    /// // A CHAdeMO-only DC post: lawful hardware, unlawful as the only
    /// // interface `[AFIR Anh. II 1.2]` — and the finding comes off the
    /// // inventory rather than off a flag somebody set.
    /// let report = assess(&profile, date!(2026 - 06 - 01));
    /// assert_eq!(
    ///     report.status_of(ObligationId::AfirAnnexIiConnector),
    ///     Some(Status::Failing)
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn profile(
        &self,
        commissioned_on: time::Date,
        accessibility: emob_core::Accessibility,
    ) -> emob_core::ChargePointProfile {
        let mut profile =
            emob_core::ChargePointProfile::bare(self.evse_id.clone(), commissioned_on);
        profile.accessibility = accessibility;
        profile.current_type = self.current_type;
        profile.rated_power_kw = self.max_power_kw;
        profile.connectors = self.connectors.iter().map(|c| c.kind).collect();
        // IEC 61851-1 Mode 2 — an ordinary socket with an in-cable control box
        // — is what a point offering *only* domestic sockets is, and it is the
        // applicability limb of `[DA-656 Anh. 2.1.3]`. Read off the interfaces
        // rather than taken as a flag, for the same reason as the rest.
        profile.domestic_socket = !self.connectors.is_empty()
            && self
                .connectors
                .iter()
                .all(|c| c.kind == ConnectorType::DomesticF);
        profile.ad_hoc_payment = self.ad_hoc_payment;
        profile.v2g = self.v2g;
        // A decommissioned point is out of service; the register's own notice
        // dates are not the inventory's to state.
        profile.registration = emob_core::Registration {
            decommissioning: (self.lifecycle == Lifecycle::Decommissioned)
                .then(|| emob_core::Notice::unreported(commissioned_on)),
            ..emob_core::Registration::default()
        };
        profile
    }

    /// Whether `[AFIR Art. 5]` treats this point as a fast one.
    ///
    /// The threshold the article turns on twice: at 50 kW and above the ad-hoc
    /// price must be based on a price per kWh, and a QR code stops satisfying
    /// the payment duty.
    #[must_use]
    pub fn is_at_least_50_kw(&self) -> bool {
        self.max_power_kw >= Decimal::from(50)
    }
}

/// A station: one grid connection, one enclosure, one or more points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Station {
    /// How the publication addresses it.
    pub facility: Facility,
    /// Who operates it — the `[LSV26 §2 Nr. 3]` *Betreiber*, and the party a
    /// `[LSV26 §4]` notice comes from.
    pub operator: PartyId,
    /// The most the station can deliver across all its points at once, in kW.
    ///
    /// Stored rather than derived, because load management makes it genuinely
    /// independent: two 150 kW points behind a 200 kW connection is the normal
    /// case, not a mistake. What it may *not* be is outside the interval the
    /// points themselves define, which [`Station::check`] enforces.
    pub total_max_power_kw: Decimal,
    /// The points, in the order they should be published.
    pub points: Vec<ChargingPoint>,
}

impl Station {
    /// A station holding points, rated at their sum.
    ///
    /// The sum is the answer for a station with no load management, which is
    /// the case a caller who has not thought about it is in.
    #[must_use]
    pub fn new(facility: Facility, operator: PartyId, points: Vec<ChargingPoint>) -> Self {
        let total_max_power_kw = points.iter().map(|point| point.max_power_kw).sum();
        Self {
            facility,
            operator,
            total_max_power_kw,
            points,
        }
    }

    /// The station total, bounded by the points it contains.
    ///
    /// # Errors
    ///
    /// [`PoiError::TotalPowerBelowPoint`] when the published total is less than
    /// one of its own outlets can deliver, and
    /// [`PoiError::TotalPowerAboveSum`] when it exceeds what they can deliver
    /// together. Both are documents a schema validator accepts and a route
    /// planner acts on.
    pub fn check(&self) -> Result<()> {
        let Some(largest) = self.points.iter().map(|point| point.max_power_kw).max() else {
            return Ok(());
        };
        if self.total_max_power_kw < largest {
            return Err(PoiError::TotalPowerBelowPoint {
                station: self.facility.id.clone(),
                total: self.total_max_power_kw.to_string(),
                point: largest.to_string(),
            });
        }
        let sum: Decimal = self.points.iter().map(|point| point.max_power_kw).sum();
        if self.total_max_power_kw > sum {
            return Err(PoiError::TotalPowerAboveSum {
                station: self.facility.id.clone(),
                total: self.total_max_power_kw.to_string(),
                sum: sum.to_string(),
            });
        }
        Ok(())
    }
}

/// A site: the place a map pin is, and the thing a driver drives to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// How the publication addresses it.
    pub facility: Facility,
    /// The name a driver would recognise.
    pub name: String,
    /// Where it is.
    pub coordinates: Coordinates,
    /// The postal address.
    pub address: Address,
    /// The zone the site's wall clock runs on.
    ///
    /// Where every local time about this site is read: the daily window a
    /// time-restricted price applies in, the opening hours, and — the reason it
    /// is not optional — the zone `[OCPI 2.3.0 §mod_locations_location_object]`
    /// makes a mandatory field of a Location and
    /// `[DATEX-II-Profil]` carries as `FacilityLocation.timeZone`.
    ///
    /// A tariff offered at this site states the same zone
    /// (`emob_tariff::Tariff::time_zone`); a tariff that states a different one
    /// prices a night rate at hours this site never sees.
    pub time_zone: TimeZone,
    /// The stations on it.
    pub stations: Vec<Station>,
}

impl Site {
    /// A site holding stations.
    #[must_use]
    pub fn new(
        facility: Facility,
        name: impl Into<String>,
        coordinates: Coordinates,
        address: Address,
        time_zone: TimeZone,
        stations: Vec<Station>,
    ) -> Self {
        Self {
            facility,
            name: name.into(),
            coordinates,
            address,
            time_zone,
            stations,
        }
    }

    /// Every point on the site, in publication order.
    pub fn points(&self) -> impl Iterator<Item = &ChargingPoint> {
        self.stations.iter().flat_map(|station| &station.points)
    }

    /// Check every station on the site.
    ///
    /// # Errors
    ///
    /// The first thing [`Station::check`] objects to.
    pub fn check(&self) -> Result<()> {
        self.stations.iter().try_for_each(Station::check)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn kw(n: i64) -> Decimal {
        Decimal::from(n)
    }

    fn point(id: &str, evse: &str, power: i64) -> ChargingPoint {
        ChargingPoint::new(
            Facility::new(id),
            EvseId::parse(evse).unwrap(),
            Connector::new(ConnectorType::Iec62196T2Combo, kw(power)),
        )
    }

    #[test]
    fn a_station_may_publish_less_than_its_points_sum_because_load_management_is_real() {
        // Two 150 kW points behind a 200 kW connection. This is not a mistake,
        // it is what a cabinet with load management does, and a model that
        // forced the sum would force operators to publish a lie.
        let mut station = Station::new(
            Facility::new("station"),
            PartyId::new("DE", "ABC").unwrap(),
            vec![
                point("p1", "DE*ABC*E00001", 150),
                point("p2", "DE*ABC*E00002", 150),
            ],
        );
        station.total_max_power_kw = kw(200);
        assert!(station.check().is_ok());
    }

    #[test]
    fn a_station_may_not_publish_less_than_one_of_its_own_outlets() {
        let mut station = Station::new(
            Facility::new("station"),
            PartyId::new("DE", "ABC").unwrap(),
            vec![point("p1", "DE*ABC*E00001", 150)],
        );
        station.total_max_power_kw = kw(50);
        assert!(matches!(
            station.check(),
            Err(PoiError::TotalPowerBelowPoint { .. })
        ));
    }

    #[test]
    fn a_station_may_not_publish_more_than_its_outlets_can_deliver() {
        // The transformer behind it may well be rated at 400 kW. The station
        // cannot deliver it, because the sockets cannot.
        let mut station = Station::new(
            Facility::new("station"),
            PartyId::new("DE", "ABC").unwrap(),
            vec![point("p1", "DE*ABC*E00001", 150)],
        );
        station.total_max_power_kw = kw(400);
        assert!(matches!(
            station.check(),
            Err(PoiError::TotalPowerAboveSum { .. })
        ));
    }

    #[test]
    fn the_current_type_follows_the_connector_rather_than_being_asserted_twice() {
        let dc = point("p", "DE*ABC*E00001", 150);
        assert_eq!(dc.current_type, CurrentType::Dc);

        let ac = ChargingPoint::new(
            Facility::new("p"),
            EvseId::parse("DE*ABC*E00002").unwrap(),
            Connector::new(ConnectorType::Iec62196T2, kw(22)),
        );
        assert_eq!(ac.current_type, CurrentType::Ac);
    }

    #[test]
    fn the_fifty_kilowatt_threshold_is_inclusive() {
        // `[AFIR Art. 5(4)]` says "equal to or more than 50 kW", and the point
        // exactly at the boundary is the one an implementation gets wrong.
        assert!(point("p", "DE*ABC*E00001", 50).is_at_least_50_kw());
        assert!(!point("p", "DE*ABC*E00001", 49).is_at_least_50_kw());
    }

    #[test]
    fn coordinates_keep_the_precision_they_arrived_with() {
        // Six decimals is about 0.1 m — one bay of a car park. This exact
        // latitude is the profile's own published example, and the binary
        // double nearest to it is 50.779598999999997488430381035: a `f64` round
        // trip moves the site, and moves it by a different amount at every
        // latitude, which is why the whole model is exact decimal.
        let latitude = Decimal::from_str_exact("50.779599").unwrap();
        assert_eq!(latitude.to_string(), "50.779599");
        assert_eq!(latitude.scale(), 6);
    }

    #[test]
    fn the_profile_the_calendar_judges_comes_off_the_inventory() {
        use emob_core::obligation::{ObligationId, Status, assess};

        // One inventory, two audiences: the same connector list that reaches
        // the national access point is the one `[AFIR Anh. II 1.2]` is judged
        // against. Before this bridge existed the second one was whatever a
        // report was handed.
        let mut post = point("p1", "DE*ABC*E00001", 150);
        let on = time::macros::date!(2026 - 06 - 01);
        let commissioned = time::macros::date!(2025 - 01 - 01);

        let profile = post.profile(commissioned, emob_core::Accessibility::Public);
        assert_eq!(profile.rated_power_kw, kw(150));
        assert_eq!(profile.current_type, CurrentType::Dc);
        assert_eq!(
            assess(&profile, on).status_of(ObligationId::AfirAnnexIiConnector),
            Some(Status::Satisfied)
        );

        // Swap the interface for one Annex II does not admit on its own and the
        // finding follows, with nothing else touched.
        post.connectors = vec![Connector::new(ConnectorType::Chademo, kw(150))];
        assert_eq!(
            assess(
                &post.profile(commissioned, emob_core::Accessibility::Public),
                on
            )
            .status_of(ObligationId::AfirAnnexIiConnector),
            Some(Status::Failing)
        );

        // A garage socket is Mode 2, and the bridge reads that off the
        // interfaces rather than taking it as a flag — which is what keeps
        // `[DA-656 Anh. 2.1.3]` off it.
        let schuko = ChargingPoint::new(
            Facility::new("p2"),
            EvseId::parse("DE*ABC*E00002").unwrap(),
            Connector::new(ConnectorType::DomesticF, kw(3)),
        );
        assert!(
            schuko
                .profile(commissioned, emob_core::Accessibility::Private)
                .domestic_socket
        );
        assert!(
            !post
                .profile(commissioned, emob_core::Accessibility::Public)
                .domestic_socket
        );
    }
}
