//! The estate's tariffs, published, and `GET /v1/tarifd` — the operator's view
//! of what the estate charges, by id and fingerprint.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::routing::get;
use emob_service::Operators;
use emob_service::estate::{EstateError, Operator, Parties, TariffSpec};
use serde::{Deserialize, Serialize};

use crate::Tarifd;

/// What `tarifd` reads of the estate.
#[derive(Debug, Clone, Deserialize)]
pub struct Estate {
    /// Whom the tariffs are published as.
    pub parties: Parties,
    /// The tariffs.
    pub tariffs: Vec<TariffSpec>,
    /// Who may read the read model.
    #[serde(default)]
    pub operators: Vec<Operator>,
}

/// A service holding every estate tariff.
///
/// # Errors
///
/// [`EstateError::Invalid`] naming `tariffs[index]` for a tariff that does not
/// build or whose history does not hold.
pub fn published(estate: &Estate) -> Result<Tarifd, EstateError> {
    // The same histories `csmsd` rates with (spec 015): one function, so the
    // version published is the version billed.
    let mut service = Tarifd::new();
    for history in emob_service::estate::histories(&estate.tariffs)?.into_values() {
        service.publish(history);
    }
    Ok(service)
}

/// One tariff, as an operator checks it against the receipt.
#[derive(Debug, Serialize)]
pub struct TariffView {
    /// Its id.
    pub id: String,
}

/// The read model.
#[derive(Debug, Serialize)]
pub struct View {
    /// The tariffs held.
    pub tariffs: Vec<TariffView>,
}

fn lock<T>(held: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    held.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `GET /v1/tarifd`, behind the operators.
pub fn router(service: Arc<Mutex<Tarifd>>, operators: Operators) -> axum::Router {
    let routes = axum::Router::new()
        .route(
            "/v1/tarifd",
            get(|State(service): State<Arc<Mutex<Tarifd>>>| async move {
                Json(View {
                    tariffs: lock(&service)
                        .tariffs()
                        .map(|id| TariffView { id: id.to_string() })
                        .collect(),
                })
            }),
        )
        .with_state(service);
    emob_service::serve::operator_only(routes, operators)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_estates_tariffs_are_published() {
        let estate: Estate =
            emob_service::estate::from_str(include_str!("../../../deploy/estate.example.toml"))
                .unwrap();
        let service = published(&estate).unwrap();
        let ids: Vec<String> = service.tariffs().map(ToString::to_string).collect();
        assert_eq!(ids, ["ad-hoc", "station-display"]);
    }
}
