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
    /// The stations.
    pub stations: Vec<Station>,
    /// Who may read the read model.
    #[serde(default)]
    pub operators: Vec<Operator>,
}

/// The estate's stations, one site per `location.id`.
///
/// # Errors
///
/// [`EstateError::Invalid`] naming `stations[index].location.time_zone`.
pub fn sites(estate: &Estate) -> Result<Vec<Site>, EstateError> {
    let mut sites: BTreeMap<String, Site> = BTreeMap::new();
    for (index, station) in estate.stations.iter().enumerate() {
        let place = &station.location;
        let point = ChargingPoint::new(
            Facility::new(station.evse.to_string()),
            station.evse.clone(),
            Connector::new(station.connector, station.rated_power_kw),
        );
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
                    facility: Facility::new(&place.id),
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
        site.stations.push(PoiStation::new(
            Facility::new(&station.identity),
            estate.parties.operator.clone(),
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
        }
    }
}

/// `GET /v1/poid`, behind the operators.
pub fn router(view: View, operators: Operators) -> axum::Router {
    let routes = axum::Router::new()
        .route(
            "/v1/poid",
            get(|State(view): State<Arc<View>>| async move { Json(view.as_ref().clone()) }),
        )
        .with_state(Arc::new(view));
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
