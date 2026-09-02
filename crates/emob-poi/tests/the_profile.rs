//! Checked against the Mobilithek's own published reference instances.
//!
//! `[DATEX-II-Profil]` ships two example documents with the profile — one per
//! publication — and they are the only artefact in this domain that says what a
//! conformant message actually *looks* like. The release does carry JSON
//! Schemas beside them, and they are the better authority on **which attributes
//! exist**; what they cannot show is the nesting a real message uses, and a
//! producer that guesses at that produces a document a consumer skips rather
//! than rejects. So the instance anchors the shape and the schema attests the
//! leaves it does not happen to exercise — see [`ATTESTED_BY_DICTIONARY`].
//!
//! So the reference is what this crate is tested against, not a fixture written
//! here. `TABLE_PATHS` and `STATUS_PATHS` below are the complete set of JSON
//! paths in `EnergyInfrastructureTablePublication_rev2.json` and
//! `EnergyInfrastructureStatusPublication_rev2.json` — derived mechanically,
//! with array indices collapsed to `[]` so a path names a *shape* rather than
//! an occurrence.
//!
//! The proposition: **every path this crate emits is a path the reference
//! instance also contains.** A misspelled key, a level of nesting too few, an
//! attribute hung on the wrong class — all of them fail here, at the one place
//! where being wrong is otherwise silent.
//!
//! The converse is deliberately *not* asserted. The reference exercises far
//! more of the profile than AFIR obliges — amenities, supplemental equipment,
//! reservation windows, planned statuses. Publishing what is obliged and no
//! more is a choice, and a test demanding the whole example would be a test
//! demanding the choice be a different one.
//!
//! # Where the instance is not the profile
//!
//! The example instance is not exhaustive either: it exercises a *sample* of
//! the attributes the dictionary defines. So a handful of the attributes this
//! crate writes are attested by the **dictionary** rather than by the example —
//! `Dictionary_DATEX_II_Profil_EnergyInfrastructureTable_01-00-00.pdf`, which
//! lists every class, attribute and multiplicity in the profile.
//!
//! [`ATTESTED_BY_DICTIONARY`] is that list, and it is short on purpose: each
//! entry is a place where the instance could not be the evidence, so each one
//! names the class it belongs to. Anything not in the instance and not in this
//! list is a mistake.
//!
//! Retrieval: <https://github.com/MobilithekDE/AFIR-DATEX-II-Recharging-Profil>,
//! `Releases/Version 01-00-00/Schema und Beispiele`, revision 2 of 10.04.2026.

use std::collections::BTreeSet;

use emob_core::{AdHocPayment, Currency, EvseId, PartyId, V2gCommunication};
use emob_poi::datex::{InformationStatus, PointUpdate, Publisher};
use emob_poi::feed::Feed;
use emob_poi::rate;
use emob_poi::site::{
    Address, ChargingPoint, Connector, ConnectorType, Coordinates, Facility, Site, Station,
};
use emob_poi::status::{PointStatus, Report};
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind, TaxIncluded};
use rust_decimal::Decimal;

include!("profile/reference_paths.rs");

/// Attributes the dictionary defines and the example instance does not use.
///
/// Every one is `0..1` or `0..*` on a class the instance does populate, so the
/// nesting is anchored by the example and only the leaf is anchored by the
/// dictionary:
///
/// | Attribute | Class | Multiplicity |
/// |---|---|---|
/// | `name` | `FacilityObject` | `0..1` `Common.MultilingualString` |
/// | `taxIncluded` | `EnergyPrice` | `0..1` `Common.Boolean` |
/// | `taxRate` | `EnergyPrice` | `0..1` `Common.Percentage` |
/// | `additionalInformation` | `EnergyPrice` | `0..1` `Common.MultilingualString` |
/// | `maximumDeliveryFee` | `EnergyRate` | `0..1` `AfirFacilities.AmountOfMoney` |
/// | `combinationWithParkingFee` | `EnergyRate` | `0..1` `Common.Boolean` |
///
/// The last one is the profile's only acknowledgement that parking might be
/// priced at all — see `emob_poi::rate` for why that matters more than its
/// multiplicity suggests.
const ATTESTED_BY_DICTIONARY: &[&str] = &[
    "/name",
    "/name/values",
    "/name/values[]/lang",
    "/name/values[]/value",
    "/energyPrice[]/taxIncluded",
    "/energyPrice[]/taxRate",
    "/energyPrice[]/additionalInformation",
    "/energyPrice[]/additionalInformation/values",
    "/energyPrice[]/additionalInformation/values[]/lang",
    "/energyPrice[]/additionalInformation/values[]/value",
    "/energyRate[]/maximumDeliveryFee",
    "/energyRate[]/minimumDeliveryFee",
    "/energyRate[]/combinationWithParkingFee",
    // A tiered tariff's bands. `timeBasedApplicability/fromMinute` is in the
    // instance; the rest are attested by the release's own JSON Schema —
    // `DATEXII_3_AfirEnergyInfrastructure.json`, where `EnergyPrice` declares
    // both applicabilities and each declares its two `NonNegativeInteger`
    // bounds. That file is a stronger contract than the example for *which
    // attributes exist*, and the example exercises a sample of them.
    "/energyPrice[]/energyBasedApplicability",
    "/energyPrice[]/energyBasedApplicability/fromKWh",
    "/energyPrice[]/energyBasedApplicability/toKWh",
    "/energyPrice[]/timeBasedApplicability",
    "/energyPrice[]/timeBasedApplicability/fromMinute",
    "/energyPrice[]/timeBasedApplicability/toMinute",
];

/// Whether a path is one the dictionary attests where the instance is silent.
fn attested(path: &str) -> bool {
    ATTESTED_BY_DICTIONARY
        .iter()
        .any(|suffix| path.ends_with(suffix))
}

fn dec(value: &str) -> Decimal {
    Decimal::from_str_exact(value).expect("a decimal literal")
}

/// Every JSON path in a document, with array indices collapsed to `[]`.
fn paths(value: &serde_json::Value) -> BTreeSet<String> {
    fn walk(value: &serde_json::Value, at: &str, out: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    let path = format!("{at}/{key}");
                    out.insert(path.clone());
                    walk(child, &path, out);
                }
            }
            serde_json::Value::Array(items) => {
                let path = format!("{at}[]");
                for child in items {
                    walk(child, &path, out);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(value, "", &mut out);
    out
}

/// A tariff exercising every price shape the crate can publish.
fn tariff() -> Tariff {
    Tariff {
        id: "ad-hoc".parse().expect("a tariff id"),
        currency: Currency::EUR,
        kind: TariffKind::AdHoc,
        tax_included: TaxIncluded::Yes,
        elements: vec![emob_tariff::TariffElement::unrestricted(vec![
            PriceComponent {
                dimension: Dimension::Energy,
                price: dec("0.49"),
                vat: Some(dec("19")),
                step_size: 1,
            },
            PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
        ])],
        min_price: None,
        max_price: Some(dec("100.00")),
        valid_from: Some(time::macros::datetime!(2026-01-01 00:00 UTC)),
        valid_until: None,
    }
}

fn site() -> Site {
    let fast = ChargingPoint {
        v2g: V2gCommunication::both_generations(),
        ..ChargingPoint::new(
            Facility::new("21F02723-POINT-1"),
            EvseId::parse("DE*ABC*E00001").expect("an EVSE id"),
            Connector::new(ConnectorType::Iec62196T2Combo, dec("150")),
        )
    };
    let slow = ChargingPoint {
        ad_hoc_payment: AdHocPayment::QrCode,
        ..ChargingPoint::new(
            Facility::new("21F02723-POINT-2"),
            EvseId::parse("DE*ABC*E00002").expect("an EVSE id"),
            Connector::new(ConnectorType::Iec62196T2, dec("22")),
        )
    };

    Site::new(
        Facility::new("21F02723-SITE"),
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
        vec![Station::new(
            Facility::new("21F02723-STATION"),
            PartyId::new("DE", "ABC").expect("a party id"),
            vec![fast, slow],
        )],
    )
}

fn feed_of(sites: Vec<Site>, rate: rate::Rate) -> (Feed<'static>, rate::Rate) {
    let published = rate.clone();
    (
        Feed {
            publisher: Publisher {
                country: "DE".to_owned(),
                national_identifier: "DE-NAP-OrganisationXY".to_owned(),
                language: "de".to_owned(),
            },
            information_status: InformationStatus::Test,
            table: Facility::new("2474A514-TABLE").at_version(2),
            table_name: Some("Region Nord".to_owned()),
            sites,
            rate_for: Box::leak(Box::new(move |_: &ChargingPoint| Some(rate.clone()))),
        },
        published,
    )
}

#[test]
fn every_path_the_table_publication_emits_is_one_the_reference_instance_contains() {
    let (published_rate, _) = rate::publish(&tariff(), "74034E3E-RATE");
    let (feed, _) = feed_of(vec![site()], published_rate);

    let json = feed
        .table(time::macros::datetime!(2026-04-14 12:00 UTC))
        .expect("a consistent feed")
        .to_json()
        .expect("serialisable");
    let emitted = paths(&serde_json::from_str(&json).expect("valid JSON"));

    let reference: BTreeSet<&str> = TABLE_PATHS.iter().copied().collect();
    let unknown: Vec<&String> = emitted
        .iter()
        .filter(|path| !reference.contains(path.as_str()) && !attested(path))
        .collect();

    assert!(
        unknown.is_empty(),
        "these paths are neither in the Mobilithek's reference instance nor \
         attested by its dictionary: {unknown:#?}"
    );
    assert!(
        emitted.len() > 40,
        "a feed this small would not be evidence of anything: {}",
        emitted.len()
    );
}

#[test]
fn a_tiered_tariff_publishes_the_band_each_price_applies_in() {
    // A price published without its condition reads as unconditional, so a
    // route planner shows the first tier's rate as *the* rate. The profile has
    // the fields; the only question is whether the whole-unit bounds are
    // narrowed the safe way.
    let tiered = Tariff {
        elements: vec![
            emob_tariff::TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.39"))],
                restrictions: emob_tariff::Restrictions {
                    max_kwh: Some(dec("10")),
                    ..emob_tariff::Restrictions::default()
                },
            },
            emob_tariff::TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.59"))],
                restrictions: emob_tariff::Restrictions {
                    min_kwh: Some(dec("10.5")),
                    min_duration_s: Some(1800),
                    ..emob_tariff::Restrictions::default()
                },
            },
        ],
        ..tariff()
    };

    let (published, notes) = rate::publish(&tiered, "74034E3E-RATE");
    assert_eq!(published.prices.len(), 2);
    assert_eq!(
        published.prices[0].energy_applicability,
        Some(rate::EnergyApplicability {
            from_kwh: None,
            to_kwh: Some(10),
        })
    );
    // 10.5 kWh has no integer spelling. `fromKWh: 10` would claim the 0.59
    // price applies over [10, 10.5), where it does not — so the bound moves the
    // other way and the note carries the figure that moved.
    assert_eq!(
        published.prices[1].energy_applicability,
        Some(rate::EnergyApplicability {
            from_kwh: Some(11),
            to_kwh: None,
        })
    );
    assert_eq!(
        published.prices[1].time_applicability,
        Some(rate::TimeApplicability {
            from_minute: Some(30),
            to_minute: None,
        })
    );
    assert!(
        notes.iter().any(|note| matches!(
            note,
            rate::RateNote::BoundNarrowedToWholeUnits {
                field: "fromKWh",
                published: 11,
                ..
            }
        )),
        "{notes:#?}"
    );

    // …and the paths it emits are paths the profile defines.
    let (feed, _) = feed_of(vec![site()], published);
    let json = feed
        .table(time::macros::datetime!(2026-04-14 12:00 UTC))
        .expect("a consistent feed")
        .to_json()
        .expect("serialisable");
    let emitted = paths(&serde_json::from_str(&json).expect("valid JSON"));
    let reference: BTreeSet<&str> = TABLE_PATHS.iter().copied().collect();
    let unknown: Vec<&String> = emitted
        .iter()
        .filter(|path| !reference.contains(path.as_str()) && !attested(path))
        .collect();
    assert!(unknown.is_empty(), "{unknown:#?}");
    assert!(
        emitted
            .iter()
            .any(|path| path.ends_with("/energyBasedApplicability/fromKWh")),
        "the band has to actually reach the document"
    );
}

#[test]
fn every_path_the_status_publication_emits_is_one_the_reference_instance_contains() {
    let (published_rate, _) = rate::publish(&tariff(), "74034E3E-RATE");
    let (feed, _) = feed_of(vec![site()], published_rate);

    let updates = vec![
        PointUpdate {
            site: Facility::new("21F02723-SITE"),
            station: Facility::new("21F02723-STATION"),
            point: Facility::new("21F02723-POINT-1"),
            report: Report::operating(PointStatus::Charging),
            price: Some(emob_poi::datex::PriceUpdate {
                rate_id: "74034E3E-RATE".to_owned(),
                price_type: "pricePerKWh",
                value: dec("0.37"),
            }),
        },
        PointUpdate {
            site: Facility::new("21F02723-SITE"),
            station: Facility::new("21F02723-STATION"),
            point: Facility::new("21F02723-POINT-2"),
            report: Report::operating(PointStatus::Available),
            price: None,
        },
    ];

    let json = feed
        .status(&updates, time::macros::datetime!(2026-04-14 12:00 UTC))
        .expect("references that resolve")
        .to_json()
        .expect("serialisable");
    let emitted = paths(&serde_json::from_str(&json).expect("valid JSON"));

    let reference: BTreeSet<&str> = STATUS_PATHS.iter().copied().collect();
    let unknown: Vec<&String> = emitted
        .iter()
        .filter(|path| !reference.contains(path.as_str()) && !attested(path))
        .collect();

    assert!(
        unknown.is_empty(),
        "these paths are neither in the Mobilithek's reference instance nor \
         attested by its dictionary: {unknown:#?}"
    );
}

#[test]
fn the_price_in_the_feed_is_the_tariffs_own_decimal_and_not_a_second_computation() {
    // `[AFIR Art. 5(2)]` and `[AFIR Art. 20(2)(c)]` are two duties about one
    // number. This is the number: `0.49`, exact, from `emob_tariff::Tariff`
    // straight into the JSON, with 19 % VAT beside it — the same rate the CDR's
    // EN 16931 breakdown uses.
    let (published_rate, notes) = rate::publish(&tariff(), "74034E3E-RATE");
    let (feed, _) = feed_of(vec![site()], published_rate);

    let json = feed
        .table(time::macros::datetime!(2026-04-14 12:00 UTC))
        .expect("a consistent feed")
        .to_json()
        .expect("serialisable");

    assert!(json.contains(r#""value": 0.49"#), "{json}");
    assert!(json.contains(r#""taxRate": 19"#), "{json}");
    assert!(json.contains(r#""taxIncluded": true"#), "{json}");
    assert!(json.contains(r#""maximumDeliveryFee": 100"#), "{json}");

    // …and the profile could not carry the occupancy fee, which is said rather
    // than swallowed.
    assert!(matches!(
        notes.as_slice(),
        [rate::RateNote::OccupancyFeeHasNoPriceType { .. }]
    ));
}

#[test]
fn kilowatts_reach_the_feed_as_the_watts_the_profile_asks_for() {
    // A 150 kW charger published as `150` is a 150-watt charger, and a route
    // planner would filter it out of every long-distance route.
    let (published_rate, _) = rate::publish(&tariff(), "r");
    let (feed, _) = feed_of(vec![site()], published_rate);

    let json = feed
        .table(time::macros::datetime!(2026-04-14 12:00 UTC))
        .expect("a consistent feed")
        .to_json()
        .expect("serialisable");

    assert!(json.contains(r#""maxPowerAtSocket": 150000"#), "{json}");
    assert!(json.contains(r#""totalMaximumPower": 172000"#), "{json}");
}

#[test]
fn the_evse_id_travels_as_the_extension_the_profile_reserves_for_it() {
    // `typeOfIdentifier` has no `evseId` literal, so the identifier type is
    // `extendedG` and the real answer sits beside it. A producer that writes
    // `"typeOfIdentifier": {"value": "evseId"}` writes a document whose
    // identifier type no consumer recognises.
    let (published_rate, _) = rate::publish(&tariff(), "r");
    let (feed, _) = feed_of(vec![site()], published_rate);

    let json = feed
        .table(time::macros::datetime!(2026-04-14 12:00 UTC))
        .expect("a consistent feed")
        .to_json()
        .expect("serialisable");

    assert!(json.contains(r#""identifier": "DE*ABC*E00001""#), "{json}");
    assert!(json.contains(r#""extendedValueG": "evseId""#), "{json}");
}

#[test]
fn a_feed_whose_station_total_contradicts_its_points_is_not_published_at_all() {
    // The check runs before serialisation, not after: a document that has been
    // written has usually been sent.
    let mut sites = vec![site()];
    sites[0].stations[0].total_max_power_kw = dec("10");

    let (published_rate, _) = rate::publish(&tariff(), "r");
    let (feed, _) = feed_of(sites, published_rate);

    assert!(matches!(
        feed.table(time::macros::datetime!(2026-04-14 12:00 UTC)),
        Err(emob_poi::PoiError::TotalPowerBelowPoint { .. })
    ));
}
