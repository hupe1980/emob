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
    poid.accepted(&snapshot, at(1));
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
    let snapshot = poid
        .snapshot(at(0))
        .expect("a coherent inventory publishes");
    poid.accepted(&snapshot, at(0));

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
        changed_at: time::OffsetDateTime::UNIX_EPOCH,
        report: site("Europe/Berlin")
            .points()
            .next()
            .expect("one point")
            .report(PointStatus::Available)
            .expect("an operating point may report `available`"),
        price: None,
    }];

    let err = poid
        .status(emob_poi::datex::Delivery::Delta, &updates, at(1))
        .expect_err("no table has been accepted yet");
    assert!(matches!(err, PublishError::NoSnapshotYet), "{err}");
    assert!(err.to_string().contains("publish the table first"), "{err}");

    // Once the table is out, the same update publishes.
    let snapshot = poid
        .snapshot(at(0))
        .expect("a coherent inventory publishes");
    poid.accepted(&snapshot, at(1));
    let status = poid
        .status(emob_poi::datex::Delivery::Delta, &updates, at(2))
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

#[test]
fn a_status_for_a_version_the_access_point_never_received_is_refused() {
    // The sharper half, and the one the service used to answer with a shrug.
    // "Has a snapshot been accepted" is not the question a status message
    // asks — *which* snapshot is. Between the last push and this update sits
    // every change the operator made, and an update built against the estate
    // resolves against the estate perfectly while naming a version the access
    // point has never had. `[DATEX-II-Profil]` gives that consumer no way to
    // ask for it, so it drops the message without a word (D308).
    let (published, _) = rate::publish(&tariff("Europe/Berlin"), "RATE-1");
    let rate_for = move |_: &ChargingPoint| Some(published.clone());
    let mut poid = Poid::new(
        publisher(),
        Facility::new("TABLE-1"),
        vec![site("Europe/Berlin")],
        &rate_for,
    );

    let snapshot = poid
        .snapshot(at(0))
        .expect("a coherent inventory publishes");
    poid.accepted(&snapshot, at(1));

    // The operator revises the point — a new connector, a corrected power, a
    // renamed station. The facility goes to version 2, and no table has gone
    // out since.
    poid.sites[0].stations[0].points[0].facility = Facility::new("SITE-1-POINT-1").at_version(2);

    let updates = vec![PointUpdate {
        site: Facility::new("SITE-1"),
        station: Facility::new("SITE-1-STATION"),
        point: Facility::new("SITE-1-POINT-1").at_version(2),
        changed_at: time::OffsetDateTime::UNIX_EPOCH,
        report: poid.sites[0]
            .points()
            .next()
            .expect("one point")
            .report(PointStatus::Available)
            .expect("an operating point may report `available`"),
        price: None,
    }];

    let refused = poid
        .status(emob_poi::datex::Delivery::Delta, &updates, at(2))
        .expect_err("the access point holds version 1");
    let PublishError::Feed(emob_poi::PoiError::NotInAcceptedTable {
        ref facility,
        ref referenced,
        ref accepted,
    }) = refused
    else {
        panic!("{refused:?}");
    };
    assert_eq!(facility, "SITE-1-POINT-1");
    assert_eq!(referenced, "2");
    assert_eq!(accepted.as_deref(), Some("1"));
    assert!(
        refused.to_string().contains("publish the table before"),
        "the remedy is on the error: {refused}"
    );

    // …and publishing the table is the remedy, exactly as the message says.
    let republished = poid
        .snapshot(at(3))
        .expect("the revised inventory publishes");
    poid.accepted(&republished, at(4));
    let status = poid
        .status(emob_poi::datex::Delivery::Delta, &updates, at(5))
        .expect("references that resolve");
    assert!(status.to_json().expect("JSON").contains("SITE-1-POINT-1"));
}

#[test]
fn a_price_update_addressed_to_a_rate_no_table_named_is_refused() {
    // `[AFIR Art. 20(2)(f)]` makes the ad-hoc price dynamic, so a change at noon
    // is a status message addressed to a rate the table already named by `idG`.
    // Nothing checked that name, and one that resolves against nothing is
    // dropped in the same silence as a version — leaving the public price the
    // old one (D308).
    let (published, _) = rate::publish(&tariff("Europe/Berlin"), "RATE-1");
    let rate_for = move |_: &ChargingPoint| Some(published.clone());
    let mut poid = Poid::new(
        publisher(),
        Facility::new("TABLE-1"),
        vec![site("Europe/Berlin")],
        &rate_for,
    );
    let snapshot = poid
        .snapshot(at(0))
        .expect("a coherent inventory publishes");
    poid.accepted(&snapshot, at(1));

    let update = |rate_id: &str| {
        vec![PointUpdate {
            site: Facility::new("SITE-1"),
            station: Facility::new("SITE-1-STATION"),
            point: Facility::new("SITE-1-POINT-1"),
            changed_at: time::OffsetDateTime::UNIX_EPOCH,
            report: poid.sites[0]
                .points()
                .next()
                .expect("one point")
                .report(PointStatus::Available)
                .expect("an operating point may report `available`"),
            price: Some(emob_poi::datex::PriceUpdate {
                rate_id: rate_id.to_owned(),
                changed_at: time::OffsetDateTime::UNIX_EPOCH,
                prices: vec![emob_poi::rate::Price {
                    price_type: emob_poi::rate::PriceType::PricePerKwh,
                    value: dec("0.37"),
                    tax_rate: None,
                    tax_included: None,
                    additional_information: None,
                    period: None,
                    energy_applicability: None,
                    time_applicability: None,
                }],
            }),
        }]
    };

    let refused = poid
        .status(emob_poi::datex::Delivery::Delta, &update("RATE-2"), at(2))
        .expect_err("no table named RATE-2");
    assert!(
        matches!(
            refused,
            PublishError::Feed(emob_poi::PoiError::RateNotInAcceptedTable { ref rate_id })
                if rate_id == "RATE-2"
        ),
        "{refused:?}"
    );

    // The name the table gave out resolves.
    poid.status(emob_poi::datex::Delivery::Delta, &update("RATE-1"), at(2))
        .expect("the rate the table named");
}

// ── …and which snapshot the access point holds survives the process ─────────
//
// The one fact this service decides. Forgotten on restart it reads as *"no
// snapshot has ever been accepted"* — which is the **safe** direction, because
// `status` then refuses rather than addressing a version nobody holds (D308).
//
// Safe is not right. A publisher that republishes its whole estate after every
// restart is one an access point rate-limits, and a fleet restarting through a
// rollout publishes nothing correct in between. This is the fourth of the four
// ledgers, and it is journalled last for exactly that reason.

use emob_service::Actor;
use emob_service::journal::{File, Journal as _};
use poid::Ledger;

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir()
            .join(format!("emob-poid-{}-{name}", std::process::id()))
            .join("poid.ndjson");
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
        Self(path)
    }

    fn ledger(&self) -> Ledger<File> {
        Ledger::open(File::open(&self.0).expect("the journal opens")).expect("the journal folds")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().expect("a parent"));
    }
}

#[test]
fn the_snapshot_the_access_point_holds_is_the_one_a_restart_reads_back() {
    let scratch = Scratch::new("held");
    let (published, _) = rate::publish(&tariff("Europe/Berlin"), "RATE-1");
    let rate_for = move |_: &ChargingPoint| Some(published.clone());

    {
        let mut poid = Poid::new(
            publisher(),
            Facility::new("TABLE-1"),
            vec![site("Europe/Berlin")],
            &rate_for,
        );
        let snapshot = poid
            .snapshot(at(0))
            .expect("a coherent inventory publishes");
        scratch
            .ledger()
            .accepted(&Actor::Timer, &mut poid, &snapshot, at(1))
            .expect("the access point took it");
    }

    // The process is gone.
    let ledger = scratch.ledger();
    let held = ledger
        .last_accepted()
        .expect("the journal folds")
        .expect("the access point holds a snapshot");
    assert_eq!(held.published_at, at(0));
    assert_eq!(held.at, at(1));

    // …and the service built around it can address a status message again,
    // which is the whole reason the version is a fact and not a flag.
    let mut poid = Poid::new(
        publisher(),
        Facility::new("TABLE-1"),
        vec![site("Europe/Berlin")],
        &rate_for,
    );
    poid.restore(held);
    assert!(
        poid.stale(at(30), time::Duration::hours(24)).is_none(),
        "a feed the access point refreshed an hour ago is not stale because this process restarted"
    );
    poid.status(emob_poi::datex::Delivery::Delta, &[], at(31))
        .expect("a status message resolves against the snapshot the access point holds");
}

#[test]
fn a_journal_edited_in_the_middle_will_not_start_the_publisher() {
    let scratch = Scratch::new("tamper");
    let (published, _) = rate::publish(&tariff("Europe/Berlin"), "RATE-1");
    let rate_for = move |_: &ChargingPoint| Some(published.clone());
    {
        let mut poid = Poid::new(
            publisher(),
            Facility::new("TABLE-1"),
            vec![site("Europe/Berlin")],
            &rate_for,
        );
        let mut ledger = scratch.ledger();
        for minute in 0..3 {
            let snapshot = poid.snapshot(at(minute)).expect("publishes");
            ledger
                .accepted(&Actor::Timer, &mut poid, &snapshot, at(minute + 1))
                .expect("taken");
        }
        assert_eq!(ledger.journal().records().expect("chained").len(), 3);
    }

    let text = std::fs::read_to_string(&scratch.0).expect("read");
    let mut lines: Vec<&str> = text.lines().collect();
    lines.remove(1);
    std::fs::write(&scratch.0, format!("{}\n", lines.join("\n"))).expect("written");

    let refused = Ledger::open(File::open(&scratch.0).expect("opens"))
        .expect_err("a journal with a record removed is not this publisher's ledger");
    assert!(refused.to_string().contains("does not chain"), "{refused}");
}
