//! Independent shared HTTP cache in front of the public point and seller routes (SR-209).
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use emob_service::Actor;
use emob_service::journal::{Journal, Kinded, Memory};
use http_cache_semantics::{BeforeRequest, CacheOptions, CachePolicy};
use time::{Duration, OffsetDateTime, macros::datetime};
use tower::ServiceExt as _;

struct NoAuthorisation;
impl csmsd::authorize::Authorize for NoAuthorisation {
    fn ask<'a>(
        &'a self,
        _: &'a str,
        _evse: Option<&'a emob_core::EvseId>,
    ) -> Pin<Box<dyn Future<Output = csmsd::authorize::Answer> + Send + 'a>> {
        panic!("a public price read must never ask about an individual card")
    }
}

#[derive(Clone)]
struct Page {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

struct SharedCache {
    routes: Router,
    entries: Vec<(CachePolicy, Page)>,
    hits: usize,
    clock: emob_service::Clock,
}

impl SharedCache {
    async fn get(&mut self, path: &str, language: &str) -> Page {
        let req = Request::get(path)
            .header(header::ACCEPT_LANGUAGE, language)
            .body(())
            .unwrap();
        let now = (self.clock)().into();
        for (policy, entry) in &self.entries {
            if let BeforeRequest::Fresh(_) = policy.before_request(&req, now) {
                self.hits += 1;
                return entry.clone();
            }
        }
        let response = self
            .routes
            .clone()
            .oneshot(req.clone().map(|()| Body::empty()))
            .await
            .unwrap();
        let policy = CachePolicy::new_options(
            &req,
            &response,
            now,
            CacheOptions {
                shared: true,
                ..Default::default()
            },
        );
        let page = Page {
            status: response.status(),
            headers: response.headers().clone(),
            body: String::from_utf8(
                to_bytes(response.into_body(), 100_000)
                    .await
                    .unwrap()
                    .to_vec(),
            )
            .unwrap(),
        };
        if policy.is_storable() {
            self.entries.push((policy, page.clone()));
        }
        page
    }
}

fn origin(
    clock: emob_service::Clock,
    before: OffsetDateTime,
) -> (Router, Arc<csmsd::Csmsd<Memory>>) {
    // Read through both daemons' normal estate configuration, including the
    // literal gross occupancy rate per minute that an operator can enter.
    let config = format!(
        "{}\n{}",
        include_str!("../../../deploy/estate.example.toml").replacen(
            "energy = \"0.49\"",
            "occupancy_per_minute = \"0.10\"\nenergy = \"0.49\"",
            1
        ),
        r#"[[tariffs]]
id = "ad-hoc"
currency = "EUR"
time_zone = "Europe/Berlin"
energy = "0.59"
vat = "19"
occupancy_per_minute = "0.20"
valid_from = "2026-11-25T12:00:00Z""#
    );
    let estate: csmsd::estate::Estate = emob_service::estate::from_str(&config).unwrap();
    let tariffs: tarifd::http::Estate = emob_service::estate::from_str(&config).unwrap();
    let service = Arc::new(Mutex::new(tarifd::http::published(&tariffs).unwrap()));
    let station = &estate.stations[0];
    assert!(station.rated_power_kw >= 50.into());
    let source = Memory::new();
    for event in [
        csmsd::journal::Event::StationRegistered {
            station: Box::new(station.clone()),
            offers: Vec::new(),
            at: before,
        },
        csmsd::journal::Event::Reserved {
            station: station.identity.clone(),
            number: 1,
            reference: "private-reference".into(),
            card: "42".into(),
            until: before + Duration::minutes(15),
            by: "pilot".into(),
            reason: "private reservation".into(),
            at: before,
        },
    ] {
        source
            .append(
                event.kind(),
                &Actor::Estate,
                &serde_json::to_string(&event).unwrap(),
            )
            .unwrap();
    }
    let csms = Arc::new(
        csmsd::Csmsd::open(
            estate.parties.operator.clone(),
            emob_eichrecht::KeyRegistry::new(),
            Arc::new(NoAuthorisation),
            source,
            emob_core::TimeZone::utc(),
        )
        .unwrap()
        .with_clock(Arc::clone(&clock))
        .pricing(csmsd::estate::tariffs(&estate).unwrap()),
    );
    let routes = csmsd::price::router(Arc::clone(&csms), csmsd::price::Operator::default())
        .merge(tarifd::driver::router_with_clock(service, clock))
        .route(
            "/cache-control-example",
            axum::routing::get(|headers: HeaderMap| async move {
                (
                    [
                        (header::CACHE_CONTROL, "public, max-age=60"),
                        (header::VARY, "Accept-Language"),
                    ],
                    headers[header::ACCEPT_LANGUAGE]
                        .to_str()
                        .unwrap()
                        .to_owned(),
                )
            }),
        );
    (routes, csms)
}

#[tokio::test]
async fn sr_209_a_shared_cache_sees_language_cutover_hold_and_retirement() {
    let boundary = datetime!(2026-11-25 12:00 UTC);
    let before = boundary - Duration::nanoseconds(1);
    let instant = Arc::new(Mutex::new(before));
    let clock: emob_service::Clock = {
        let instant = Arc::clone(&instant);
        Arc::new(move || *instant.lock().unwrap())
    };
    let (routes, csms) = origin(Arc::clone(&clock), before);
    let mut cache = SharedCache {
        routes,
        clock,
        entries: Vec::new(),
        hits: 0,
    };
    // A cache that always calls the origin could otherwise make every assertion pass.
    for language in ["en", "de", "en", "de"] {
        assert_eq!(
            cache.get("/cache-control-example", language).await.body,
            language
        );
    }
    assert_eq!(cache.hits, 2);
    let point = "/p/DE*ABC*E00001";
    let seller = "/v1/driver-tariffs/ad-hoc";
    let de = cache.get(point, "de").await;
    assert_eq!(de.headers[header::VARY], "Accept-Language");
    assert!(
        de.body
            .contains("<h1 class=\"price\">0,49\u{a0}€ pro kWh</h1>")
    );
    assert!(de.body.contains("<li>0,10\u{a0}€ pro Minute Belegung</li>"));
    assert!(de.body.contains("Reserviert bis"));
    let en = cache.get(point, "en").await;
    assert!(
        en.body
            .contains("<h1 class=\"price\">0.49\u{a0}€ per kWh</h1>")
    );
    assert!(en.body.contains("<li>0.10\u{a0}€ per minute occupied</li>"));
    assert!(en.body.contains("Reserved until") && !en.body.contains("private-reference"));
    csms.cancel_reservation(&Actor::Estate, "CP-1", 1)
        .await
        .unwrap();
    assert!(!cache.get(point, "en").await.body.contains("Reserved until"));
    let feed = format!("{point}.atom");
    assert!(
        cache
            .get(&feed, "de")
            .await
            .body
            .contains("0,10\u{a0}€ pro Minute Belegung")
    );
    let old = cache.get(seller, "en").await;
    let old_json: serde_json::Value = serde_json::from_str(&old.body).unwrap();
    let old_url = format!("{seller}/{}", old_json["version"].as_str().unwrap());
    assert_eq!(old_json["components"][0]["price"], "0.49");
    assert_eq!(old_json["components"][1]["dimension"], "PARKING_TIME");
    assert_eq!(old_json["components"][1]["price"], "6.00");
    assert_eq!(old_json["components"][1]["unit"], "hour");
    assert_eq!(cache.get(&old_url, "en").await.body, old.body);
    *instant.lock().unwrap() = boundary;
    let current = cache.get(point, "en").await;
    assert!(
        current
            .body
            .contains("<h1 class=\"price\">0.59\u{a0}€ per kWh</h1>")
    );
    assert!(
        current
            .body
            .contains("<li>0.20\u{a0}€ per minute occupied</li>")
    );
    *instant.lock().unwrap() = boundary + Duration::nanoseconds(1);
    let next: serde_json::Value =
        serde_json::from_str(&cache.get(seller, "en").await.body).unwrap();
    assert_ne!(next["version"], old_json["version"]);
    assert_eq!(next["components"][0]["price"], "0.59");
    assert_eq!(cache.get(&old_url, "en").await.body, old.body);
    csms.retire(&Actor::Estate, "CP-1", "pilot retirement")
        .unwrap();
    assert_eq!(cache.get(point, "en").await.status, StatusCode::NOT_FOUND);
    assert_eq!(cache.get(point, "de").await.status, StatusCode::NOT_FOUND);
    assert_eq!(cache.get(&feed, "de").await.status, StatusCode::NOT_FOUND);
    assert_eq!(cache.get(&old_url, "de").await.body, old.body);
    assert_eq!(
        cache.hits, 2,
        "only the positive control was served from shared storage"
    );
}
