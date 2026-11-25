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
        status_max_age_secs: 86400,
        dynamic_url: url.replace("/42", "/43"),
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
        status_max_age_secs: 86400,
        dynamic_url: "https://mobilithek.example/publication/43".to_owned(),
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

/// Read the wire documents as a subscriber. A failed second page must never erase a point.
/// Replay only the HTTP documents, without access to the publisher's registry.
fn check_subscriber_references(documents: &[(String, serde_json::Value)]) {
    use std::collections::BTreeMap;
    fn objects<'a>(
        node: &'a serde_json::Value,
        found: &mut Vec<&'a serde_json::Map<String, serde_json::Value>>,
    ) {
        match node {
            serde_json::Value::Array(values) => {
                for child in values {
                    objects(child, found);
                }
            }
            serde_json::Value::Object(value) => {
                if value.contains_key("idG") {
                    found.push(value);
                }
                for child in value.values() {
                    objects(child, found);
                }
            }
            _ => {}
        }
    }

    let mut accepted = BTreeMap::new();
    let mut checked = 0;
    for (offer, doc) in documents {
        let mut found = Vec::new();
        objects(doc, &mut found);
        if offer == "static" {
            accepted.clear();
            for object in found.iter().filter(|o| !o.contains_key("targetClass")) {
                accepted.insert(
                    object["idG"].clone().to_string(),
                    object.get("versionG").cloned(),
                );
            }
        } else {
            for object in found.iter().filter(|o| o.contains_key("targetClass")) {
                let version = accepted.get(&object["idG"].to_string()).expect(
                    "subscriber can resolve the referenced ID in its last accepted static table",
                );
                if let Some(expected) = object.get("versionG") {
                    assert_eq!(version.as_ref(), Some(expected));
                }
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 5,
        "table, site, station, point and rate references reached the receiver"
    );
}

#[tokio::test]
async fn sr_207_sr_208_two_offers_keep_their_own_acknowledgements() {
    use axum::routing::{get, post};
    use emob_service::publication::{ConnectorObservation, PublicPoint, RegistryPage};
    use poid::live::{Checkpoint, Worker};
    use std::sync::{
        RwLock,
        atomic::{AtomicBool, Ordering},
    };
    use time::{Duration, OffsetDateTime};

    let mut estate = estate();
    let station = &estate.stations[0];
    let when = OffsetDateTime::now_utc() - Duration::minutes(3);
    let point = PublicPoint {
        identity: station.identity.clone(),
        evse: station.evse.clone(),
        location: station.location.clone(),
        connector: station.connector,
        rated_power_kw: station.rated_power_kw,
        tariff: station.tariff.clone(),
        version: 1,
        site_version: 1,
        changed_at: when,
        fully_renewable: Some(true),
        status_changed_at: None,
        observation: Some(ConnectorObservation {
            status: "Available".into(),
            at: when,
        }),
    };
    let source = Arc::new(Mutex::new(point));
    let seen = Arc::new(Mutex::new(Vec::<(String, serde_json::Value)>::new()));
    let unavailable = Arc::new(AtomicBool::new(false));
    let incomplete = Arc::new(AtomicBool::new(false));
    let retire = Arc::new(AtomicBool::new(false));
    let hold_reply = Arc::new(AtomicBool::new(false));
    let reject_static = Arc::new(AtomicBool::new(true));
    let release = Arc::new(tokio::sync::Notify::new());
    let app = axum::Router::new()
        .route(
            "/v1/csmsd/public-points",
            get({
                let (source, incomplete, retire) =
                    (source.clone(), incomplete.clone(), retire.clone());
                move |headers: axum::http::HeaderMap, uri: axum::http::Uri| {
                    let (source, incomplete, retire) =
                        (source.clone(), incomplete.clone(), retire.clone());
                    async move {
                        assert_eq!(headers["authorization"], "Bearer read-registry");
                        if uri.query().is_some() {
                            return Err(StatusCode::CONFLICT);
                        }
                        Ok(axum::Json(RegistryPage {
                            revision: 1,
                            points: (!retire.load(Ordering::SeqCst))
                                .then(|| source.lock().unwrap().clone())
                                .into_iter()
                                .collect(),
                            next: incomplete.load(Ordering::SeqCst).then_some(1),
                        }))
                    }
                }
            }),
        )
        .route(
            "/offers/{audience}",
            post({
                let reject_static = reject_static.clone();
                let (seen, unavailable, hold_reply, release) = (
                    seen.clone(),
                    unavailable.clone(),
                    hold_reply.clone(),
                    release.clone(),
                );
                move |axum::extract::Path(audience): axum::extract::Path<String>,
                      headers: axum::http::HeaderMap,
                      axum::Json(doc): axum::Json<serde_json::Value>| {
                    let reject_static = reject_static.clone();
                    let (seen, unavailable, hold_reply, release) = (
                        seen.clone(),
                        unavailable.clone(),
                        hold_reply.clone(),
                        release.clone(),
                    );
                    async move {
                        assert!(!headers.contains_key("authorization"));
                        if audience == "static" && reject_static.load(Ordering::SeqCst) {
                            return StatusCode::SERVICE_UNAVAILABLE;
                        }
                        if audience == "dynamic" && unavailable.load(Ordering::SeqCst) {
                            return StatusCode::SERVICE_UNAVAILABLE;
                        }
                        let wait =
                            audience == "dynamic" && hold_reply.swap(false, Ordering::SeqCst);
                        seen.lock().unwrap().push((audience, doc));
                        if wait {
                            release.notified().await;
                        }
                        StatusCode::NO_CONTENT
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    estate.desk.csmsd.clone_from(&base);
    let config = emob_service::estate::Mobilithek {
        status_max_age_secs: 86400,
        url: format!("{base}/offers/static"),
        dynamic_url: format!("{base}/offers/dynamic"),
        client_cert: None,
        client_key: None,
    };
    let receiver = poid::push::client(&config).unwrap();
    estate.mobilithek = Some(config);
    let view = Arc::new(RwLock::new(poid::http::View::of(&[], None)));
    let mut worker = Worker {
        ledger: poid::Ledger::open(emob_service::journal::Memory::default()).unwrap(),
        publisher: Publisher {
            country: "DE".into(),
            national_identifier: "TEST".into(),
            language: "de".into(),
        },
        table: Facility::new("TABLE"),
        estate,
        receiver,
        source: reqwest::Client::new(),
        token: emob_service::Secret::new("read-registry"),
        checkpoint: Checkpoint::default(),
        held: None,
    };
    assert!(worker.round(&view).await.is_err());
    assert!(seen.lock().unwrap().is_empty());
    worker.checkpoint = worker.ledger.last_dynamic().unwrap();
    assert_eq!(worker.checkpoint.delivery.pending_since, Some(when));
    assert!(worker.ledger.last_accepted().unwrap().is_none());
    assert_eq!(worker.checkpoint.delivery.static_attempts, 1);
    assert!(worker.checkpoint.delivery.static_accepted_at.is_none());
    reject_static.store(false, Ordering::SeqCst);
    worker.round(&view).await.unwrap();
    {
        let docs = seen.lock().unwrap();
        assert_eq!(docs[0].0, "static");
        assert_eq!(docs[1].0, "dynamic");
        assert!(docs[0].1.to_string().contains("\"isGreenEnergy\":true"));
        let initial = docs[1].1.to_string();
        assert!(initial.contains("snapshotPush") && initial.contains("available"));
    }
    assert_eq!(
        worker
            .ledger
            .last_dynamic()
            .unwrap()
            .delivery
            .missed_deadlines,
        1
    );
    source.lock().unwrap().observation = Some(ConnectorObservation {
        status: "Occupied".into(),
        at: OffsetDateTime::now_utc(),
    });
    incomplete.store(true, Ordering::SeqCst);
    assert!(worker.round(&view).await.is_err());
    assert_eq!(seen.lock().unwrap().len(), 2);
    incomplete.store(false, Ordering::SeqCst);
    unavailable.store(true, Ordering::SeqCst);
    assert!(worker.round(&view).await.is_err());
    worker.checkpoint = worker.ledger.last_dynamic().unwrap();
    unavailable.store(false, Ordering::SeqCst);
    worker.round(&view).await.unwrap();
    let before = seen.lock().unwrap()[2].1.to_string();
    assert!(
        before.contains("deltaPush")
            && before.contains("occupied")
            && !before.contains("energyRateUpdate")
    );
    worker.estate.tariffs[0].energy = rust_decimal::Decimal::ZERO;
    worker.estate.tariffs[0].session_fee = Some(rust_decimal::Decimal::ONE);
    worker.round(&view).await.unwrap();
    {
        let docs = seen.lock().unwrap();
        let delta = docs[3].1.to_string();
        assert_eq!(docs.len(), 4);
        assert!(
            delta.contains("deltaPush")
                && delta.contains("energyRateUpdate")
                && delta.contains("occupied")
        );
        assert!(delta.contains("DE*ABC*E00001-ADHOC"));
        let path = "/messageContainer/payload/0/aegiEnergyInfrastructureStatusPublication/energyInfrastructureSiteStatus/0/energyInfrastructureStationStatus/0/refillPointStatus/0/aegiElectricChargingPointStatus";
        let status = docs[2].1.pointer(path).unwrap();
        let price = docs[3].1.pointer(path).unwrap();
        assert!(status.get("lastUpdated").is_some());
        assert_eq!(price["lastUpdated"], status["lastUpdated"]);
        let components = price["energyRateUpdate"][0]["energyPrice"]
            .as_array()
            .unwrap();
        assert_eq!(components.len(), 2);
        for amount in [0, 1] {
            assert!(components.iter().any(|p| {
                p["value"].to_string().parse::<rust_decimal::Decimal>().ok() == Some(amount.into())
            }));
        }
        assert_eq!(worker.checkpoint.delivery.attempts, 4);
        assert_eq!(worker.checkpoint.delivery.static_attempts, 2);
        assert!(worker.checkpoint.delivery.static_accepted_at.is_some());
    }
    // An expired but previously authoritative observation becomes unknown.
    source.lock().unwrap().observation.as_mut().unwrap().at = when;
    worker
        .estate
        .mobilithek
        .as_mut()
        .unwrap()
        .status_max_age_secs = 10;
    worker.round(&view).await.unwrap();
    let unknown = seen.lock().unwrap().last().unwrap().1.to_string();
    assert!(unknown.contains("unknown") && unknown.contains("deltaPush"));
    assert!(!unknown.contains("available"));

    // The receiver saw a change but held its response beyond our timeout.
    worker.receiver = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(300))
        .build()
        .unwrap();
    hold_reply.store(true, Ordering::SeqCst);
    source.lock().unwrap().observation = Some(ConnectorObservation {
        status: "Faulted".into(),
        at: OffsetDateTime::now_utc(),
    });
    assert!(worker.round(&view).await.is_err());
    worker.checkpoint = worker.ledger.last_dynamic().unwrap();
    assert!(worker.checkpoint.delivery.pending_since.is_some());
    source.lock().unwrap().observation = Some(ConnectorObservation {
        status: "Available".into(),
        at: OffsetDateTime::now_utc(),
    });
    worker.round(&view).await.unwrap();
    let accepted = worker.checkpoint.delivery.accepted_at;
    release.notify_one();
    worker.round(&view).await.unwrap();
    assert_eq!(
        worker.ledger.last_dynamic().unwrap().delivery.accepted_at,
        accepted
    );
    assert!(
        seen.lock()
            .unwrap()
            .last()
            .unwrap()
            .1
            .to_string()
            .contains("available")
    );

    // A refresh due on the same round as a change must follow its delta.
    worker.checkpoint.delivery.changes = 3499;
    source.lock().unwrap().observation = Some(ConnectorObservation {
        status: "Reserved".into(),
        at: OffsetDateTime::now_utc(),
    });
    worker.round(&view).await.unwrap();
    {
        let docs = seen.lock().unwrap();
        let n = docs.len();
        assert!(docs[n - 2].1.to_string().contains("deltaPush"));
        assert!(docs[n - 1].1.to_string().contains("snapshotPush"));
        assert!(docs[n - 1].1.to_string().contains("reserved"));
    }
    assert_eq!(worker.checkpoint.delivery.changes, 0);
    retire.store(true, Ordering::SeqCst);
    worker.round(&view).await.unwrap();
    assert!(worker.ledger.last_dynamic().unwrap().points.is_empty());
    assert!(
        worker
            .checkpoint
            .delivery
            .problem
            .as_ref()
            .unwrap()
            .contains("Withdraw")
    );
    let empty = seen.lock().unwrap().last().unwrap().1.to_string();
    assert!(empty.contains("snapshotPush") && !empty.contains("DE*ABC*E00001"));
    retire.store(false, Ordering::SeqCst);
    source.lock().unwrap().version += 1;
    worker.round(&view).await.unwrap();
    assert_eq!(worker.ledger.last_dynamic().unwrap().points.len(), 1);
    assert_eq!(
        worker
            .ledger
            .last_accepted()
            .unwrap()
            .unwrap()
            .published
            .version_of("TABLE"),
        Some("2")
    );
    worker.held = worker.ledger.last_accepted().unwrap();
    worker.checkpoint = worker.ledger.last_dynamic().unwrap();
    let before = seen.lock().unwrap().len();
    worker.round(&view).await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), before);
    // A fresh confirmation of the same state must not reset the change clock.
    let transition = OffsetDateTime::now_utc() - Duration::seconds(2);
    {
        let mut point = source.lock().unwrap();
        point.status_changed_at = Some(transition);
        point.observation = Some(ConnectorObservation {
            status: "Available".into(),
            at: transition,
        });
    }
    worker.round(&view).await.unwrap();
    let before = seen.lock().unwrap().len();
    source.lock().unwrap().observation.as_mut().unwrap().at = OffsetDateTime::now_utc();
    worker.round(&view).await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), before);
    assert_eq!(
        worker.checkpoint.delivery.last_source_change,
        Some(transition)
    );
    check_subscriber_references(&seen.lock().unwrap());
}
