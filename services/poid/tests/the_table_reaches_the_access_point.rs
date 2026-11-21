//! Spec 016: the table, priced with the tariff that rates, posted to the
//! Mobilithek — and each of its documented answers named.

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use emob_poi::datex::Publisher;
use emob_poi::site::Facility;
use poid::Poid;
use poid::push::Refused;

/// A Mobilithek on loopback: it keeps every body and answers `status`.
async fn mock(status: StatusCode) -> (String, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&seen);
    let app = axum::Router::new().route(
        "/publication/42",
        axum::routing::post(move |body: String| {
            let kept = Arc::clone(&kept);
            async move {
                kept.lock().unwrap().push(body);
                (status, "a reason the access point gave")
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/publication/42", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

fn estate() -> poid::http::Estate {
    emob_service::estate::from_str(include_str!("../../../deploy/estate.example.toml")).unwrap()
}

async fn push_to(status: StatusCode) -> (Result<(), Refused>, Vec<String>) {
    let (url, seen) = mock(status).await;
    let estate = estate();
    let now = time::macros::datetime!(2026-12-01 10:00 UTC);
    let rates = poid::rates(&estate, now).unwrap();
    let rate_for = |point: &emob_poi::site::ChargingPoint| rates.get(&point.evse_id).cloned();
    let service = Poid::new(
        Publisher {
            country: "DE".to_owned(),
            national_identifier: "DE-NAP-TEST".to_owned(),
            language: "de".to_owned(),
        },
        Facility::new("DE*ABC-TABLE"),
        poid::http::sites(&estate).unwrap(),
        &rate_for,
    );
    let publication = service.snapshot(now).unwrap();
    let client = poid::push::client(&emob_service::estate::Mobilithek {
        url: url.clone(),
        client_cert: None,
        client_key: None,
    })
    .unwrap();
    let outcome = poid::push::push(&client, &url, &publication).await;
    let seen = seen.lock().unwrap().clone();
    (outcome, seen)
}

#[tokio::test]
async fn the_table_the_mobilithek_takes_carries_the_price_that_rates() {
    let (outcome, seen) = push_to(StatusCode::OK).await;
    assert_eq!(outcome, Ok(()));
    assert_eq!(seen.len(), 1, "one POST per publication");
    let body = &seen[0];
    assert!(body.contains("DE*ABC*E00001"), "the point: {body}");
    assert!(
        body.contains("0.49"),
        "the price the estate's tariff rates with: {body}"
    );
}

#[tokio::test]
async fn each_documented_answer_is_named_and_only_an_outage_is_retried() {
    for (status, expected) in [
        (StatusCode::BAD_REQUEST, "refused the document"),
        (StatusCode::FORBIDDEN, "certificate"),
        (StatusCode::NOT_FOUND, "no publication"),
        (StatusCode::SERVICE_UNAVAILABLE, "sent again"),
    ] {
        let (outcome, _) = push_to(status).await;
        let refused = outcome.unwrap_err();
        assert!(
            refused.to_string().contains(expected),
            "{status}: {refused}"
        );
        assert_eq!(
            refused.retries(),
            status == StatusCode::SERVICE_UNAVAILABLE,
            "{status}"
        );
    }
}

#[test]
fn a_mobilithek_off_loopback_without_tls_or_a_certificate_is_refused_by_key() {
    let plain = emob_service::estate::Mobilithek {
        url: "http://mobilithek.example/publication/42".to_owned(),
        client_cert: None,
        client_key: None,
    };
    assert!(
        poid::push::client(&plain)
            .unwrap_err()
            .to_string()
            .contains("mobilithek.url")
    );
    let no_cert = emob_service::estate::Mobilithek {
        url: "https://mobilithek.example/publication/42".to_owned(),
        ..plain
    };
    assert!(
        poid::push::client(&no_cert)
            .unwrap_err()
            .to_string()
            .contains("mobilithek.client_cert")
    );
}
