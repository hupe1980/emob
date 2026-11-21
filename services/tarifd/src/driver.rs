//! `driver-tariff.v1` — a tariff as a seller imports it instead of typing it
//! (spec 016, asked for by esales).
//!
//! A system that sells a driver contract — a web shop, a fleet portal, the
//! sibling `esales` — should hold a reference to the tariff, not a copy of its
//! price. So each version is one JSON document at a stable, versioned
//! address:
//!
//! - `GET /v1/driver-tariffs/{id}`: the version in force now; cacheable for a
//!   minute.
//! - `GET /v1/driver-tariffs/{id}/{version}`: that version for ever, because a
//!   contract sold on it still refers to it.
//!
//! Every figure in it is `emob-tariff`'s: the components with their unit and
//! basis, the lines a driver must be shown, worked totals rated by the
//! function that bills, and the tariff itself as `emob-tariff` serialises it,
//! so a seller can recompute any session to the last digit. Public, and with
//! nothing about any customer in it. The schema is
//! `schemas/driver-tariff.v1.json`.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use emob_tariff::{Dimension, Tariff, TariffHistory};
use rust_decimal::Decimal;
use serde::Serialize;

use crate::Tarifd;

/// The worked examples: energy in kWh, at a power in kW.
pub const EXAMPLES: [(u32, u32); 3] = [(10, 22), (30, 50), (50, 150)];

/// One version, as a seller reads it.
#[derive(Debug, Serialize)]
pub struct DriverTariff {
    /// Always `driver-tariff.v1`.
    pub schema: &'static str,
    /// The tariff's id, stable across versions.
    pub id: String,
    /// This version: the tariff's SHA-256 fingerprint.
    pub version: String,
    /// The four characters a driver compares on the price page, the display
    /// and the receipt.
    pub preisstand: String,
    /// In force from, when it has a start.
    #[serde(with = "time::serde::rfc3339::option")]
    pub valid_from: Option<time::OffsetDateTime>,
    /// In force until (exclusive), when a later version replaces it.
    #[serde(with = "time::serde::rfc3339::option")]
    pub valid_until: Option<time::OffsetDateTime>,
    /// ISO 4217.
    pub currency: String,
    /// Every component of the version's first element.
    pub components: Vec<Component>,
    /// What a driver must be shown, in `[AFIR Art. 5(4)]`'s order.
    pub display: Vec<String>,
    /// Sessions rated by the function that bills.
    pub examples: Vec<Example>,
    /// The tariff as `emob-tariff` serialises it — what a price check
    /// recomputes with.
    pub tariff: Tariff,
}

/// One price component.
#[derive(Debug, Serialize)]
pub struct Component {
    /// `ENERGY`, `TIME`, `PARKING_TIME` or `FLAT`.
    pub dimension: Dimension,
    /// The price, exactly as the tariff states it.
    pub price: Decimal,
    /// `kWh`, `hour` or `session`.
    pub unit: &'static str,
    /// `gross` (VAT included) or `net`.
    pub basis: &'static str,
    /// The VAT rate in percent, when stated.
    pub vat: Option<Decimal>,
}

/// A worked total.
#[derive(Debug, Serialize)]
pub struct Example {
    /// Energy, kWh.
    pub kwh: Decimal,
    /// Power, kW.
    pub power_kw: Decimal,
    /// What it costs, rounded as an invoice line is.
    pub total: Decimal,
}

/// The document for one version, as it was in force from its start.
#[must_use]
pub fn document(tariff: &Tariff) -> DriverTariff {
    let at = tariff
        .valid_from
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let basis = match tariff.tax_included {
        emob_tariff::TaxIncluded::No => "net",
        _ => "gross",
    };
    DriverTariff {
        schema: "driver-tariff.v1",
        id: tariff.id.to_string(),
        version: tariff.fingerprint().to_string(),
        preisstand: tariff.fingerprint().code(),
        valid_from: tariff.valid_from,
        valid_until: tariff.valid_until,
        currency: tariff.currency.to_string(),
        components: tariff
            .elements
            .first()
            .map(|element| {
                element
                    .components
                    .iter()
                    .map(|component| Component {
                        dimension: component.dimension,
                        price: component.price,
                        unit: match component.dimension {
                            Dimension::Energy => "kWh",
                            Dimension::Time | Dimension::ParkingTime => "hour",
                            Dimension::Flat => "session",
                        },
                        basis,
                        vat: component.vat,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        display: emob_tariff::describe_gross(tariff, at)
            .map(|shown| shown.full_disclosure())
            .unwrap_or_default(),
        examples: EXAMPLES
            .iter()
            .filter_map(|&(kwh, power)| {
                let (kwh, power_kw) = (Decimal::from(kwh), Decimal::from(power));
                let total = emob_tariff::worked_total(tariff, kwh, power_kw, at)?;
                Some(Example {
                    kwh,
                    power_kw,
                    total: total.amount(),
                })
            })
            .collect(),
        tariff: tariff.clone(),
    }
}

fn lock<T>(held: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    held.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn answer(tariff: &Tariff, cache: &'static str) -> Response {
    let mut response = Json(document(tariff)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    response
}

/// The public routes. Nothing in them is personal; an unknown id or version
/// is an empty 404.
pub fn router(service: Arc<Mutex<Tarifd>>) -> axum::Router {
    axum::Router::new()
        .route(
            "/v1/driver-tariffs/{id}",
            get(
                |State(service): State<Arc<Mutex<Tarifd>>>, Path(id): Path<String>| async move {
                    let now = time::OffsetDateTime::now_utc();
                    let held = lock(&service);
                    match held
                        .history(&id)
                        .and_then(|history| history.in_force_at(now))
                    {
                        Some(tariff) => answer(tariff, "public, max-age=60"),
                        None => StatusCode::NOT_FOUND.into_response(),
                    }
                },
            ),
        )
        .route(
            "/v1/driver-tariffs/{id}/{version}",
            get(
                |State(service): State<Arc<Mutex<Tarifd>>>,
                 Path((id, version)): Path<(String, String)>| async move {
                    let held = lock(&service);
                    match held
                        .history(&id)
                        .and_then(|history| find(history, &version))
                    {
                        // A version never changes.
                        Some(tariff) => answer(tariff, "public, max-age=31536000, immutable"),
                        None => StatusCode::NOT_FOUND.into_response(),
                    }
                },
            ),
        )
        .with_state(service)
}

fn find<'a>(history: &'a TariffHistory, version: &str) -> Option<&'a Tariff> {
    history
        .versions()
        .iter()
        .find(|tariff| tariff.fingerprint().to_string() == version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_is_a_document_the_published_schema_accepts() {
        let estate: crate::http::Estate =
            emob_service::estate::from_str(include_str!("../../../deploy/estate.example.toml"))
                .unwrap();
        let service = crate::http::published(&estate).unwrap();
        let tariff = &service.history("ad-hoc").unwrap().versions()[0];
        let document = serde_json::to_value(document(tariff)).unwrap();
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../schemas/driver-tariff.v1.json")).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&document)
            .map(|e| format!("{e} at {}", e.instance_path()))
            .collect();
        assert!(errors.is_empty(), "{errors:?}\n{document:#}");
        assert_eq!(document["preisstand"].as_str().unwrap().len(), 4);
        assert_eq!(document["display"][0], "0.49 EUR / kWh");
        // 30 kWh at 0.49, rated by the function that bills.
        assert_eq!(document["examples"][1]["total"], "14.70");
    }
}
