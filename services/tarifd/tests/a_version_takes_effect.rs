//! M3c, for the half that publishes a price.
//!
//! > A tariff version takes effect on a schedule, the feed says so within the
//! > hour, and every 2.1 station in the estate is holding the new version — the
//! > payload is `emob_ocpp::to_ocpp`, so the service decides *when* and never
//! > *what*.
//!
//! The last clause is the one worth testing, because it is the one a service
//! quietly stops honouring: a display string typed into a configuration file, a
//! rounded figure in an export job. So these assert that the three payloads the
//! service hands out read **one decimal** — and that the number is the tariff's
//! own, not one this service composed.

use emob_core::{Currency, PartyId, TimeZone};
use emob_tariff::{
    Dimension, PriceComponent, Tariff, TariffElement, TariffHistory, TariffKind, TaxIncluded,
};
use rust_decimal::Decimal;
use std::str::FromStr;
use tarifd::{Audience, PublishError, Tarifd};
use time::macros::datetime;

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn party() -> PartyId {
    PartyId::new("DE", "ABC").unwrap()
}

fn at(minutes: i64) -> time::OffsetDateTime {
    datetime!(2026-06-01 00:00 +2) + time::Duration::minutes(minutes)
}

/// The price every audience is owed: 0.49 gross per kWh.
fn version(from: Option<time::OffsetDateTime>, until: Option<time::OffsetDateTime>) -> Tariff {
    Tariff::simple(
        "ad-hoc".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        TimeZone::new("Europe/Berlin").unwrap(),
        vec![PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19"))],
    )
    .valid_between(from, until)
}

/// …and the one that replaces it at 10:00.
fn successor() -> Tariff {
    Tariff::simple(
        "ad-hoc".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        TimeZone::new("Europe/Berlin").unwrap(),
        vec![PriceComponent::new(Dimension::Energy, dec("0.59")).with_vat(dec("19"))],
    )
    .valid_between(Some(at(600)), None)
}

fn service() -> Tarifd {
    let mut tarifd = Tarifd::new();
    tarifd.publish(
        TariffHistory::new(vec![version(None, Some(at(600))), successor()])
            .expect("two versions that partition the timeline"),
    );
    tarifd
}

#[test]
fn one_version_reaches_three_audiences_at_one_decimal() {
    // The claim the whole service exists to keep. Every payload is built by the
    // crate that owns the crossing, from the one `Tariff` that rates the CDR —
    // so there is no second computation for the three to drift from, and this
    // asserts the consequence rather than the intention.
    let tarifd = service();
    let publication = tarifd
        .prepare(&successor(), &party(), at(540))
        .expect("a lawful tariff crosses to all three");

    // The station's own screen `[AFIR Art. 5(4)]`. OCPP quotes prices excluding
    // tax, so 0.59 gross at 19 % is 0.59 / 1.19.
    let station = publication
        .station
        .energy
        .as_ref()
        .expect("an energy price reaches the point");
    let net = dec(&station.prices[0].price_kwh.to_string());
    assert_eq!(net.round_dp(6), (dec("0.59") / dec("1.19")).round_dp(6));

    // The national access point `[AFIR Art. 20(2)(c)]`, which carries the gross
    // figure and the rate beside it.
    let published = &publication.national_access_point.prices[0];
    assert_eq!(published.value, dec("0.59"));
    assert_eq!(published.tax_rate, Some(dec("19")));
    assert_eq!(published.tax_included, Some(true));

    // …and the roaming partner, which carries the gross price per component.
    let partner = &publication.partner.elements[0].price_components[0];
    assert_eq!(partner.price.get(), dec("0.59"));

    // One decimal, three documents: the station's net grosses back up to the
    // figure the other two publish.
    assert_eq!((net * dec("1.19")).round_dp(2), published.value);
}

#[test]
fn a_version_is_due_before_it_takes_effect_and_late_after_it() {
    // `[AFIR Art. 5(4)]` requires the price to be known to the driver **before**
    // they initiate a session. A publication that goes out when the version
    // takes effect is already late for everybody standing at a point at that
    // instant — so the service looks forward, and "late" is a different and
    // sharper question from "due".
    let mut tarifd = service();
    let lead = time::Duration::hours(1);

    // At 08:00 the successor takes effect in two hours: not due yet.
    let due = tarifd.due(at(480), lead);
    assert!(
        !due.iter().any(|t| t.valid_from == Some(at(600))),
        "two hours out is beyond a one-hour lead"
    );

    // At 09:30 it is inside the lead, and nobody has been told.
    let due = tarifd.due(at(570), lead);
    assert!(due.iter().any(|t| t.valid_from == Some(at(600))));

    // Nothing is late yet: the version in force is the first one… which nobody
    // has been told about either. That is the point of the distinction.
    let late = tarifd.late(at(570));
    assert_eq!(late.len(), 3, "three audiences, one version in force");
    assert!(late.iter().all(|l| l.effective_at.is_none()));

    // Tell all three about the version in force, and nothing is late.
    let first = tarifd
        .prepare(&version(None, Some(at(600))), &party(), at(570))
        .unwrap();
    for audience in Audience::ALL {
        tarifd.confirm(&first, audience, at(570));
    }
    assert!(tarifd.late(at(570)).is_empty());

    // …and at 10:00 the successor is in force and was never published, which is
    // a breach with a name rather than a backlog item.
    let late = tarifd.late(at(600));
    assert_eq!(late.len(), 3);
    let said = late[0].to_string();
    assert!(said.contains("took effect"), "{said}");
    assert!(
        said.contains("shown one price and billed another"),
        "{said}"
    );
    assert!(
        late.iter()
            .any(|l| l.to_string().contains("[AFIR Art. 5(4)]")),
        "the station's duty names its article"
    );
}

#[test]
fn a_push_that_failed_stays_late_rather_than_being_forgotten() {
    // Recording a delivery is a separate act from attempting one, and this is
    // why: an estate that did not receive a version is charging a price its
    // stations do not display, and the service has to be able to say so.
    let mut tarifd = service();
    let publication = tarifd.prepare(&successor(), &party(), at(570)).unwrap();

    // The national access point accepted it; the stations did not answer.
    tarifd.confirm(&publication, Audience::NationalAccessPoint, at(575));
    tarifd.confirm(&publication, Audience::Partner, at(575));

    let late = tarifd.late(at(600));
    assert_eq!(late.len(), 1, "{late:?}");
    assert_eq!(late[0].audience, Audience::Station);
    assert_eq!(late[0].fingerprint, successor().fingerprint());

    // …and it stays due, so the next run tries again.
    assert!(
        tarifd
            .due(at(600), time::Duration::hours(1))
            .iter()
            .any(|t| t.fingerprint() == successor().fingerprint())
    );

    // Once the stations confirm, both questions go quiet.
    tarifd.confirm(&publication, Audience::Station, at(605));
    assert!(tarifd.late(at(610)).is_empty());
    assert!(tarifd.due(at(610), time::Duration::hours(1)).is_empty());
}

#[test]
fn a_version_the_stations_cannot_be_given_is_published_to_nobody() {
    // The one design decision here worth arguing about. OCPP 2.1's time price
    // is `priceMinute`, and an hourly rate with no exact per-minute spelling
    // has no representation at all — 2.50 an hour is 0.041666… a minute, and a
    // rounded figure is a price the station charges and the tariff does not.
    //
    // Publishing the other two anyway would leave the national access point and
    // every roaming partner quoting a price the estate's own stations do not
    // charge. A driver comparing on a map would be misled by a document this
    // operator signed off, which is worse than publishing nothing.
    let unshowable = Tariff::simple(
        "occupancy".parse().unwrap(),
        Currency::EUR,
        TariffKind::AdHoc,
        TimeZone::new("Europe/Berlin").unwrap(),
        vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
            PriceComponent::new(Dimension::ParkingTime, dec("2.50")).with_vat(dec("19")),
        ],
    );
    let mut tarifd = Tarifd::new();
    tarifd.publish(TariffHistory::single(unshowable.clone()).unwrap());

    let err = tarifd
        .prepare(&unshowable, &party(), at(0))
        .expect_err("an hourly rate with no exact per-minute spelling has no OCPP form");
    assert!(matches!(err, PublishError::Station(_)), "{err}");
    assert!(
        err.to_string().contains("no audience is"),
        "the refusal has to say it is total: {err}"
    );

    // …and nothing was recorded, so it is late the moment it is in force.
    assert_eq!(tarifd.late(at(1)).len(), 3);
}

#[test]
fn a_version_republished_unchanged_is_already_published() {
    // Keyed by content, so a redeployment of the same numbers is the same
    // publication — and an edit under the same id is a different one. The same
    // argument `Cdr::was_priced_with` makes one layer down: a tariff id is a
    // name, and names get reused.
    let mut tarifd = service();
    let publication = tarifd.prepare(&successor(), &party(), at(570)).unwrap();
    for audience in Audience::ALL {
        tarifd.confirm(&publication, audience, at(570));
    }
    assert!(tarifd.due(at(600), time::Duration::hours(1)).is_empty());

    // The same numbers, built again: nothing to do.
    assert_eq!(successor().fingerprint(), publication.fingerprint);
    assert_eq!(tarifd.confirmed(&successor()).len(), 3);

    // A different price under the same id is a version nobody has been told
    // about, and the service says so rather than treating the id as the
    // identity.
    let edited = Tariff {
        elements: vec![TariffElement::unrestricted(vec![
            PriceComponent::new(Dimension::Energy, dec("0.69")).with_vat(dec("19")),
        ])],
        tax_included: TaxIncluded::Yes,
        ..successor()
    };
    assert_ne!(edited.fingerprint(), publication.fingerprint);
    assert!(tarifd.confirmed(&edited).is_empty());
}

#[test]
fn the_account_of_what_every_crossing_cost_is_one_document() {
    // Three seams, one report. An operator reading why a station cannot show a
    // tier and why OCPI rounded a bound should be reading one thing, and each
    // note says which audience it is about.
    let tiered = Tariff {
        id: "tiered".parse().unwrap(),
        currency: Currency::EUR,
        kind: TariffKind::AdHoc,
        time_zone: TimeZone::new("Europe/Berlin").unwrap(),
        tax_included: TaxIncluded::Yes,
        elements: (0..12)
            .map(|i| TariffElement {
                components: vec![
                    PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
                ],
                restrictions: emob_tariff::Restrictions {
                    min_kwh: Some(Decimal::from(i)),
                    ..emob_tariff::Restrictions::default()
                },
            })
            .collect(),
        min_price: None,
        max_price: None,
        valid_from: None,
        valid_until: None,
    };
    let mut tarifd = Tarifd::new();
    tarifd.publish(TariffHistory::single(tiered.clone()).unwrap());

    let publication = tarifd.prepare(&tiered, &party(), at(0)).unwrap();
    assert!(
        publication
            .notes
            .iter()
            .any(|note| note.pointer.starts_with("/station")),
        "twelve tiers do not fit OCPP's ten-line description: {:?}",
        publication.notes
    );
}

#[test]
fn a_tariff_this_service_does_not_publish_is_refused() {
    let tarifd = Tarifd::new();
    let err = tarifd
        .prepare(&successor(), &party(), at(0))
        .expect_err("a service publishes what it was given and nothing else");
    assert!(matches!(err, PublishError::UnknownTariff { .. }), "{err}");
}
