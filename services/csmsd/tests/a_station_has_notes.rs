//! Spec 023: notes on a station — bounded in the fold, numbered once.

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
fn sr_154_twelve_notes_leave_the_newest_ten_and_numbers_are_never_reused() {
    let tariff = emob_service::estate::TariffSpec {
        id: "ad-hoc".to_owned(),
        currency: emob_core::Currency::EUR,
        time_zone: "Europe/Berlin".to_owned(),
        energy: rust_decimal::Decimal::new(49, 2),
        vat: rust_decimal::Decimal::from(19),
        per_minute: None,
        session_fee: None,
        valid_from: None,
    }
    .to_tariff(0)
    .unwrap();
    let tariff = emob_tariff::TariffHistory::single(tariff).unwrap();
    let csmsd = Csmsd::open(
        PartyId::new("DE", "ABC").unwrap(),
        KeyRegistry::new(),
        Arc::new(Nobody),
        Memory::new(),
        emob_core::TimeZone::new("Europe/Berlin").unwrap(),
    )
    .unwrap()
    .pricing(BTreeMap::from([("ad-hoc".to_owned(), tariff)]));
    csmsd.seed(&[station(1)]).unwrap();
    let by = emob_service::Actor::operator("demo").unwrap();
    for n in 1..=12 {
        assert_eq!(
            csmsd.note(&by, "CP-00001", &format!("note {n}")).unwrap(),
            n
        );
    }
    let known = csmsd.known("CP-00001").unwrap();
    let numbers: Vec<u64> = known.notes.iter().map(|note| note.number).collect();
    assert_eq!(numbers, (3..=12).collect::<Vec<_>>());
    assert!(csmsd.remove_note(&by, "CP-00001", 12).unwrap());
    assert_eq!(
        csmsd.note(&by, "CP-00001", "after a removal").unwrap(),
        13,
        "a removed note's number is not given out again"
    );
    assert!(
        !csmsd.remove_note(&by, "CP-00001", 1).unwrap(),
        "already dropped"
    );
    assert_eq!(
        csmsd.note(&by, "CP-99999", "x").unwrap_err().field,
        "identity"
    );
}
