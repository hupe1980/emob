//! M3c, for the half that publishes an estate.
//!
//! > A `snapshotPush` reaches a Mobilithek sandbox and comes back accepted.
//!
//! The sandbox itself is the one leg CI cannot run — it is somebody else's
//! server, behind credentials — so what is asserted here is everything up to
//! the socket: the document this service builds, the contradictions it refuses
//! to publish, and the record of what came back accepted. The daemon does the
//! push, exactly as `csmsd` does the WebSocket.
//!
//! The properties that matter are the ones a publishing service loses quietly:
//! a feed nobody refreshed that route planners read as current, and a dynamic
//! update that references a table version no consumer ever received.

use emob_core::{Currency, EvseId, PartyId, TimeZone};
use emob_poi::datex::{InformationStatus, PointUpdate, Publisher};
use emob_poi::rate;
use emob_poi::site::{
    Address, ChargingPoint, Connector, ConnectorType, Coordinates, Facility, Site, Station,
};
use emob_poi::status::PointStatus;
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
use poid::{Poid, PublishError};
use rust_decimal::Decimal;
use std::str::FromStr;
use time::macros::datetime;

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn at(minutes: i64) -> time::OffsetDateTime {
    datetime!(2026-04-14 12:00 UTC) + time::Duration::minutes(minutes)
}

fn publisher() -> Publisher {
    Publisher {
        country: "DE".to_owned(),
        national_identifier: "DE-NAP-0001".to_owned(),
        language: "de".to_owned(),
    }
}

fn site(zone: &str) -> Site {
    let fast = ChargingPoint::new(
        Facility::new("SITE-1-POINT-1"),
        EvseId::parse("DE*ABC*E00001").expect("an EVSE id"),
        Connector::new(ConnectorType::Iec62196T2Combo, dec("150")),
    );
    Site::new(
        Facility::new("SITE-1"),
        "Musterstadt Nord",
        Coordinates {
            latitude: dec("50.779599"),
            longitude: dec("6.104507"),
        },
        Address {
            street: "Hauptstraße".to_owned(),
            house_number: "12".to_owned(),
            postcode: "55555".to_owned(),
            city: "Musterstadt".to_owned(),
            country_code: "DE".to_owned(),
        },
        TimeZone::new(zone).expect("an IANA zone"),
        vec![Station::new(
            Facility::new("SITE-1-STATION"),
            PartyId::new("DE", "ABC").expect("a party id"),
            vec![fast],
        )],
    )
}

/// The tariff whose price the feed publishes — a night rate, so the zone it is
/// read in is load-bearing.
fn tariff(zone: &str) -> Tariff {
    Tariff::simple(
        "ad-hoc".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        TimeZone::new(zone).expect("an IANA zone"),
        vec![PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19"))],
    )
}

#[test]
fn a_snapshot_is_built_from_the_operators_own_inventory_and_recorded_when_accepted() {
    let (published, _) = rate::publish(&tariff("Europe/Berlin"), "RATE-1");
    let rate_for = move |_: &ChargingPoint| Some(published.clone());
    let mut poid = Poid::new(
        publisher(),
        Facility::new("TABLE-1"),
        vec![site("Europe/Berlin")],
        &rate_for,
    );

    // Nothing has been published, so the feed is stale from the first instant:
    // every point this operator runs is absent from the national access point.
    let stale = poid
        .stale(at(0), time::Duration::hours(24))
        .expect("never published");
    assert_eq!(stale.last_accepted, None);
    assert!(
        stale.to_string().contains("never accepted a snapshot"),
        "{stale}"
    );

    let snapshot = poid
        .snapshot(at(0))
        .expect("a coherent inventory publishes");
    let json = snapshot.to_json().expect("the profile's own JSON");
    assert!(
        json.contains("aegiEnergyInfrastructureTablePublication"),
        "{json}"
    );
    assert!(json.contains("DE-NAP-0001"), "the national identifier");
    assert!(json.contains("SITE-1-POINT-1"), "the point itself");

    // …and the push comes back accepted. That is the daemon's socket; what the
    // service owns is the record, and it is written after the answer.
    poid.accepted(at(0), at(1));
    assert!(poid.stale(at(30), time::Duration::hours(24)).is_none());
    assert_eq!(poid.last_accepted().unwrap().published_at, at(0));
}

#[test]
fn a_feed_nobody_refreshed_is_named_rather_than_read_as_current() {
    // The failure mode of published data: nothing errors, and the map is simply
    // wrong. `[AFIR Art. 20(2)]` makes this an operator's duty, and a route
    // planner has no way to tell a current feed from a stalled one.
    let (published, _) = rate::publish(&tariff("Europe/Berlin"), "RATE-1");
    let rate_for = move |_: &ChargingPoint| Some(published.clone());
    let mut poid = Poid::new(
        publisher(),
        Facility::new("TABLE-1"),
        vec![site("Europe/Berlin")],
        &rate_for,
    );
    poid.accepted(at(0), at(0));

    let within = time::Duration::hours(1);
    assert!(
        poid.stale(at(30), within).is_none(),
        "half an hour is fresh"
    );

    let stale = poid
        .stale(at(95), within)
        .expect("thirty-five minutes overdue");
    assert_eq!(stale.overdue_by, time::Duration::minutes(35));
    assert_eq!(stale.last_accepted, Some(at(0)));
    assert!(stale.to_string().contains("[AFIR Art. 20(2)]"), "{stale}");
}

#[test]
fn a_status_message_before_a_snapshot_is_refused_rather_than_dropped_in_silence() {
    // A status message addresses a facility **at the version the table
    // published it at**, and the profile gives a consumer no way to resolve a
    // reference to a version it never received. So the message is discarded
    // without a word: the charger reads `available` here and is missing from
    // every map, and nothing fails anywhere.
    let (published, _) = rate::publish(&tariff("Europe/Berlin"), "RATE-1");
    let rate_for = move |_: &ChargingPoint| Some(published.clone());
    let mut poid = Poid::new(
        publisher(),
        Facility::new("TABLE-1"),
        vec![site("Europe/Berlin")],
        &rate_for,
    );
    let updates = vec![PointUpdate {
        site: Facility::new("SITE-1"),
        station: Facility::new("SITE-1-STATION"),
        point: Facility::new("SITE-1-POINT-1"),
        // Off the point the feed publishes: `ChargingPoint::report` is the
        // only constructor, so a status is checked against the register that
        // will carry it.
        report: site("Europe/Berlin")
            .points()
            .next()
            .expect("one point")
            .report(PointStatus::Available)
            .expect("an operating point may report `available`"),
        price: None,
    }];

    let err = poid
        .status(&updates, at(1))
        .expect_err("no table has been accepted yet");
    assert!(matches!(err, PublishError::NoSnapshotYet), "{err}");
    assert!(err.to_string().contains("publish the table first"), "{err}");

    // Once the table is out, the same update publishes.
    poid.accepted(at(0), at(1));
    let status = poid
        .status(&updates, at(2))
        .expect("references that resolve");
    let json = status.to_json().expect("the profile's own JSON");
    assert!(json.contains("SITE-1-POINT-1"), "{json}");
}

#[test]
fn a_price_published_at_a_site_on_another_clock_is_not_published_at_all() {
    // A well-formed document, a lawful tariff, a real site — and a night price
    // that starts an hour after the driver standing there thinks it does.
    // Nothing fails until somebody compares a bill against a map, which is
    // exactly why the feed refuses before it is sent rather than after.
    let (published, _) = rate::publish(&tariff("Europe/Berlin"), "RATE-1");
    let rate_for = move |_: &ChargingPoint| Some(published.clone());
    let poid = Poid::new(
        publisher(),
        Facility::new("TABLE-1"),
        // The site is in Lisbon; the price was written in Berlin.
        vec![site("Europe/Lisbon")],
        &rate_for,
    );

    let err = poid
        .snapshot(at(0))
        .expect_err("a rate on the wrong clock is a price nobody at that site is charged");
    assert!(matches!(err, PublishError::Feed(_)), "{err}");
    assert!(
        err.to_string().contains("Europe/Berlin") && err.to_string().contains("Europe/Lisbon"),
        "the refusal names both clocks: {err}"
    );
}

#[test]
fn a_test_feed_and_a_real_one_are_different_documents() {
    // `informationStatus` is not decoration: a consumer routes on `real` and
    // discards `test`. A production feed published as `test` is invisible, and a
    // test feed published as `real` sends drivers to a fiction.
    let (published, _) = rate::publish(&tariff("Europe/Berlin"), "RATE-1");
    let rate_for = move |_: &ChargingPoint| Some(published.clone());
    let mut poid = Poid::new(
        publisher(),
        Facility::new("TABLE-1"),
        vec![site("Europe/Berlin")],
        &rate_for,
    );

    let real = poid.snapshot(at(0)).unwrap().to_json().unwrap();
    assert!(real.contains("\"real\""), "{real}");

    poid.information_status = InformationStatus::Test;
    let test = poid.snapshot(at(0)).unwrap().to_json().unwrap();
    assert!(test.contains("\"test\""));
    assert_ne!(real, test, "the two are different documents");
}
