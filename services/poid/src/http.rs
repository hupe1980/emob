//! The estate's stations as the sites `poid` publishes, and `GET /v1/poid`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::routing::get;
use emob_poi::site::{
    Address, ChargingPoint, Connector, Coordinates, Facility, Site, Station as PoiStation,
};
use emob_service::Operators;
use emob_service::estate::{EstateError, Operator, Parties, Station};
use serde::{Deserialize, Serialize};

use crate::Accepted;

/// What `poid` reads of the estate.
#[derive(Debug, Clone, Deserialize)]
pub struct Estate {
    /// The operator the stations are published under.
    pub parties: Parties,
    /// Internal CSMS origin and operator credential reference.
    pub desk: emob_service::estate::Desk,
    /// The stations.
    pub stations: Vec<Station>,
    /// Who may read the read model.
    #[serde(default)]
    pub operators: Vec<Operator>,
    /// The tariffs the stations name — the feed's prices (spec 016).
    #[serde(default)]
    pub tariffs: Vec<emob_service::estate::TariffSpec>,
    /// Where the table is posted; absent, nothing is sent.
    #[serde(default)]
    pub mobilithek: Option<emob_service::estate::Mobilithek>,
}

/// The estate's stations, one site per `location.id`.
///
/// # Errors
///
/// [`EstateError::Invalid`] naming `stations[index].location.time_zone`.
pub fn sites(estate: &Estate) -> Result<Vec<Site>, EstateError> {
    let points: Vec<_> = estate
        .stations
        .iter()
        .filter(|s| s.publicly_accessible)
        .map(|s| emob_service::publication::PublicPoint {
            fully_renewable: s.fully_renewable,
            identity: s.identity.clone(),
            evse: s.evse.clone(),
            location: s.location.clone(),
            connector: s.connector,
            rated_power_kw: s.rated_power_kw,
            tariff: s.tariff.clone(),
            version: 1,
            site_version: 1,
            changed_at: time::OffsetDateTime::UNIX_EPOCH,
            status_changed_at: None,
            observation: None,
        })
        .collect();
    registry_sites(&estate.parties, &points)
}

/// Assemble a table from one complete CSMS registry revision.
///
/// # Errors
/// Reports an invalid site zone or conflicting definitions of one site.
pub fn registry_sites(
    parties: &Parties,
    points: &[emob_service::publication::PublicPoint],
) -> Result<Vec<Site>, EstateError> {
    let mut sites: BTreeMap<String, Site> = BTreeMap::new();
    for (index, station) in points.iter().enumerate() {
        let place = &station.location;
        let mut point = ChargingPoint::new(
            Facility::new(station.evse.to_string()).at_version(station.version),
            station.evse.clone(),
            Connector::new(station.connector, station.rated_power_kw),
        );
        point.fully_renewable = station.fully_renewable;
        if !sites.contains_key(&place.id) {
            let time_zone = emob_core::TimeZone::new(&place.time_zone).map_err(|error| {
                EstateError::Invalid {
                    key: format!("stations[{index}].location.time_zone"),
                    reason: error.to_string(),
                }
            })?;
            sites.insert(
                place.id.clone(),
                Site {
                    facility: Facility::new(&place.id).at_version(station.site_version),
                    name: place.name.clone(),
                    coordinates: Coordinates {
                        latitude: place.latitude,
                        longitude: place.longitude,
                    },
                    address: Address {
                        street: place.street.clone(),
                        house_number: place.house_number.clone(),
                        postcode: place.postal_code.clone(),
                        city: place.city.clone(),
                        country_code: place.country.clone(),
                    },
                    time_zone,
                    stations: Vec::new(),
                },
            );
        }
        let site = sites
            .get_mut(&place.id)
            .unwrap_or_else(|| unreachable!("inserted above"));
        if site.facility.version != station.site_version
            || site.name != place.name
            || site.time_zone.name() != place.time_zone
            || site.coordinates.latitude != place.latitude
            || site.coordinates.longitude != place.longitude
            || site.address.street != place.street
            || site.address.house_number != place.house_number
            || site.address.postcode != place.postal_code
            || site.address.city != place.city
            || site.address.country_code != place.country
        {
            return Err(EstateError::Invalid {
                key: format!("stations[{index}].location"),
                reason: "two points disagree about a shared site".to_owned(),
            });
        }
        site.stations.push(PoiStation::new(
            Facility::new(&station.identity).at_version(station.version),
            parties.operator.clone(),
            vec![point],
        ));
    }
    Ok(sites.into_values().collect())
}

/// One published point.
#[derive(Debug, Clone, Serialize)]
pub struct PointView {
    /// The EVSE.
    pub evse: String,
    /// The site it stands at.
    pub location: String,
}

/// The read model.
#[derive(Debug, Clone, Serialize)]
pub struct View {
    /// The points the feed publishes.
    pub points: Vec<PointView>,
    /// What the access point last accepted, if anything.
    pub last_accepted: Option<Accepted>,
    /// Last dynamic delivery, source latency and any blocking condition.
    pub dynamic: crate::live::DeliveryState,
}

impl View {
    /// The view of these sites and that acceptance.
    #[must_use]
    pub fn of(sites: &[Site], last_accepted: Option<Accepted>) -> Self {
        Self {
            points: sites
                .iter()
                .flat_map(|site| {
                    site.points().map(|point| PointView {
                        evse: point.evse_id.to_string(),
                        location: site.facility.id.clone(),
                    })
                })
                .collect(),
            last_accepted,
            dynamic: crate::live::DeliveryState::default(),
        }
    }
}

/// The read model as the publish loop keeps it current.
pub type Shared = Arc<std::sync::RwLock<View>>;

/// `GET /v1/poid`, behind the operators.
pub fn router(view: Shared, operators: Operators) -> axum::Router {
    let routes = axum::Router::new()
        .route(
            "/v1/poid",
            get(|State(view): State<Shared>| async move {
                Json(
                    view.read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                )
            }),
        )
        .route(
            "/v1/poid/dynamic",
            get(|State(view): State<Shared>| async move {
                Json(
                    view.read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .dynamic
                        .clone(),
                )
            }),
        )
        .with_state(view);
    emob_service::serve::operator_only(routes, operators)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_estates_station_is_one_published_point() {
        let estate: Estate =
            emob_service::estate::from_str(include_str!("../../../deploy/estate.example.toml"))
                .unwrap();
        let sites = sites(&estate).unwrap();
        let view = View::of(&sites, None);
        assert_eq!(view.points.len(), 1);
        assert_eq!(view.points[0].evse, "DE*ABC*E00001");
        assert_eq!(view.points[0].location, "LOC-1");
    }
}
