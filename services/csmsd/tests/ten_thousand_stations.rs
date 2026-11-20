//! Spec 021, SC-002: the registry at the size of a real network.
//!
//! Ten thousand stations seeded into one CSMS, then searched the way the
//! station page's search box does. The bound is the release build's (50 ms);
//! a debug build is held to ten times that, so the test still catches a
//! quadratic step rather than a slow machine. The fleet is spread over
//! Germany, so the map (spec 022) has an extent to lay its grid over.

use std::collections::BTreeMap;
use std::sync::Arc;

use csmsd::{Answer, Authorize, Csmsd};
use emob_core::PartyId;
use emob_eichrecht::registry::KeyRegistry;
use emob_service::estate::Station;
use emob_service::journal::Memory;

struct Nobody;

impl Authorize for Nobody {
    fn ask<'a>(
        &'a self,
        _uid: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Answer> + Send + 'a>> {
        Box::pin(async { Answer::invalid("no provider in this test") })
    }
}

fn station(n: usize) -> Station {
    // A grid of 77 × 90 sites, 0.1° apart, from 47.3° N 6.0° E.
    let latitude = 4730 + (n % 77) * 10;
    let longitude = 600 + (n / 77 % 90) * 10;
    serde_json::from_value(serde_json::json!({
        "identity": format!("CP-{n:05}"),
        "security_profile": 1,
        "password_hash": "$argon2id$v=19$m=19456,t=2,p=1$q1iothavEHNDTeGVTzaU5A$FPoYufVFH6pj8V/ENPoPB3jm497gMXbSv9Y82oKyfYA",
        "ocpp": if n.is_multiple_of(3) { "1.6" } else { "2.0.1" },
        "evse": format!("DE*ABC*E{n:05}"),
        "rated_power_kw": "22",
        "tariff": "ad-hoc",
        "connector": "iec62196_t2",
        "location": {
            "id": format!("LOC-{}", n / 10), "name": "Standort", "street": "Ringstraße",
            "house_number": "7", "postal_code": "52062", "city": "Aachen", "country": "DE",
            "latitude": format!("{}.{:02}", latitude / 100, latitude % 100),
            "longitude": format!("{}.{:02}", longitude / 100, longitude % 100), "time_zone": "Europe/Berlin"
        }
    }))
    .expect("a station")
}

#[test]
fn ten_thousand_stations_are_searched_in_one_pass() {
    let tariff = emob_service::estate::TariffSpec {
        id: "ad-hoc".to_owned(),
        currency: emob_core::Currency::EUR,
        time_zone: "Europe/Berlin".to_owned(),
        energy: rust_decimal::Decimal::new(49, 2),
        vat: rust_decimal::Decimal::from(19),
    }
    .to_tariff(0)
    .unwrap();
    let csmsd = Arc::new(
        Csmsd::open(
            PartyId::new("DE", "ABC").unwrap(),
            KeyRegistry::new(),
            Arc::new(Nobody),
            Memory::new(),
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
        )
        .unwrap()
        .pricing(BTreeMap::from([("ad-hoc".to_owned(), tariff)])),
    );
    let fleet: Vec<Station> = (0..10_000).map(station).collect();
    assert_eq!(csmsd.seed(&fleet).unwrap(), 10_000);

    let bound = if cfg!(debug_assertions) { 500 } else { 50 };
    let started = std::time::Instant::now();
    let page = csmsd::read::search_registry(&csmsd, "e04242", None, 0, 50);
    let took = started.elapsed().as_millis();
    assert_eq!(page.total, 1, "one EVSE matches");
    assert!(took < bound, "a search over 10 000 stations took {took} ms");

    let started = std::time::Instant::now();
    let page = csmsd::read::search_registry(&csmsd, "1.6", None, 0, 200);
    assert_eq!(page.items.len(), 200, "a page, never the whole fleet");
    assert!(page.total > 3_000);
    assert!(started.elapsed().as_millis() < bound);

    // Spec 022: the whole fleet is a bounded grid, and a cell's area is a search.
    let started = std::time::Instant::now();
    let map = csmsd::read::map_of(&csmsd, "", None);
    assert!(started.elapsed().as_millis() < bound);
    assert_eq!(map.total, 10_000);
    assert!(map.points.is_empty(), "too many for dots");
    assert!(
        map.cells.len() <= 256,
        "SR-151: {} cells for 10 000 stations",
        map.cells.len()
    );
    assert_eq!(
        map.cells.iter().map(|cell| cell.stations).sum::<u64>(),
        10_000
    );
    let cell = &map.cells[0];
    let area = csmsd::read::Area::parse(&format!(
        "{},{},{},{}",
        cell.west, cell.south, cell.east, cell.north
    ))
    .unwrap();
    let inside = csmsd::read::search_registry(&csmsd, "", Some(area), 0, 200);
    assert_eq!(
        inside.total, cell.stations,
        "a cell links to exactly its stations"
    );
    let zoomed = csmsd::read::map_of(&csmsd, "", Some(area));
    assert_eq!(
        zoomed.points.len() as u64,
        cell.stations,
        "a cell drills down to dots"
    );
}

#[test]
fn sr_151_an_area_that_is_not_four_degrees_in_order_is_refused() {
    for bad in ["a,b", "6,47", "15,47,6,55", "6,47,15,95", "6;47;15;55"] {
        assert!(csmsd::read::Area::parse(bad).is_err(), "{bad}");
    }
    assert!(csmsd::read::Area::parse("5.9,47.3,15.0,55.1").is_ok());
}
