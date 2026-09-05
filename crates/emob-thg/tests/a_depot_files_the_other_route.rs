//! `[38k §7]` — the route a depot files, and the paragraph a bus is worth a
//! third more under.
//!
//! Every figure here is checked against the Verordnung's own text rather than
//! against this crate's arithmetic: the two deadlines, the seven-step factor
//! schedule `[38k §7(6)]` states, the once-per-vehicle rule, and the four
//! documents `[38k §7(1)–(2)]` asks a filer to hold.

use emob_thg::{
    Attribution, DriveEfficiency, EmissionsBasis, Estimate, EstimateClaimBuilder,
    RegistrationEvidence, Route, ThgError, VehicleClass,
};
use rust_decimal::Decimal;
use std::str::FromStr;
use time::macros::date;

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).expect("a decimal literal")
}

fn basis() -> EmissionsBasis {
    EmissionsBasis::grid_average(dec("119.4"), "BAnz AT 31.10.2026 B5").expect("a basis")
}

fn builder(year: i32) -> EstimateClaimBuilder {
    EstimateClaimBuilder::new(
        year,
        Attribution::own("DEPOT"),
        basis(),
        DriveEfficiency::BatteryElectric,
    )
    .expect("a countable year")
}

fn bus_estimate() -> Estimate {
    // A depot bus draws far more than a car; the figure is the filer's, from
    // the notice named beside it.
    Estimate::announced(
        dec("72000"),
        VehicleClass::HeavyM3OrN3,
        "BAnz AT 01.02.2027 B3",
    )
    .expect("an estimate")
}

fn car_estimate() -> Estimate {
    Estimate::announced(dec("2000"), VehicleClass::Other, "BAnz AT 01.02.2027 B3")
        .expect("an estimate")
}

/// `[38k §7(6)]` states **seven** steps against `[38k §5(3)]`'s three, and the
/// widest gap is a third more counted energy on the same kilowatt-hours.
#[test]
fn the_heavy_schedule_is_its_own_schedule_and_it_starts_in_2027() {
    let heavy = |year| VehicleClass::HeavyM3OrN3.factor(year);
    let other = |year| VehicleClass::Other.factor(year);

    // "ab dem Kalenderjahr 2027": before it the deviation has not begun, so
    // § 5(3)'s schedule is the one that applies to both.
    assert_eq!(heavy(2026), other(2026));
    assert_eq!(heavy(2026), Some(dec("3")));

    assert_eq!(heavy(2027), Some(dec("4")));
    assert_eq!(other(2027), Some(dec("3")));

    for (year, expected) in [
        (2035, "3.5"),
        (2036, "3"),
        (2037, "2.5"),
        (2038, "2"),
        (2039, "1.5"),
        (2040, "1"),
        (2050, "1"),
    ] {
        assert_eq!(heavy(year), Some(dec(expected)), "{year}");
    }

    // Neither schedule reaches back before the first counted year.
    assert_eq!(heavy(2023), None);
    assert_eq!(other(2023), None);
}

/// A mixed fleet is counted at **two** factors in one notification.
///
/// `[38k §7(4) S. 1]` multiplies the number of vehicles by the estimate, and
/// `[38k §7(6)]` multiplies by a factor that is a fact about the vehicle — so
/// the multiplication is per vehicle and not on the total.
#[test]
fn a_mixed_fleet_is_counted_at_two_factors() {
    let mut claim = builder(2027);
    for id in ["bus-1", "bus-2"] {
        claim
            .vehicle(
                id,
                "DEPOT",
                VehicleClass::HeavyM3OrN3,
                RegistrationEvidence::complete(),
                &bus_estimate(),
            )
            .expect("a countable bus");
    }
    claim
        .vehicle(
            "van-1",
            "DEPOT",
            VehicleClass::Other,
            RegistrationEvidence::complete(),
            &car_estimate(),
        )
        .expect("a countable van");

    let filed = claim.build().expect("a notification").value;

    // 2 × 72 MWh + 1 × 2 MWh.
    assert_eq!(filed.megawatt_hours(), dec("146"));
    // …and the counting: 144 × 4 + 2 × 3.
    assert_eq!(filed.counted_megawatt_hours(), dec("582"));
    assert_eq!(
        filed.deadline(),
        date!(2027 - 11 - 15),
        "`[38k §8(1) S. 1 Nr. 2]` files inside the obligation year"
    );
    assert!(!filed.in_time(date!(2027 - 11 - 16)));
}

/// A vehicle counted twice is the paragraph's own refusal.
///
/// `[38k §7(4) S. 2]`: *"Die Anrechnung … kann pro reinem
/// Batterieelektrofahrzeug und pro Verpflichtungsjahr nur einmal erfolgen."*
#[test]
fn a_vehicle_is_counted_once_per_obligation_year() {
    let mut claim = builder(2027);
    claim
        .vehicle(
            "bus-1",
            "DEPOT",
            VehicleClass::HeavyM3OrN3,
            RegistrationEvidence::complete(),
            &bus_estimate(),
        )
        .expect("a countable bus");

    let again = claim.vehicle(
        "bus-1",
        "DEPOT",
        VehicleClass::HeavyM3OrN3,
        RegistrationEvidence::complete(),
        &bus_estimate(),
    );
    assert!(
        matches!(again, Err(ThgError::VehicleAlreadyCounted { ref reference }) if reference == "bus-1"),
        "{again:?}"
    );
}

/// Each of `[38k §7(1)–(2)]`'s conditions is a refusal that names the document
/// the filer is missing.
#[test]
fn every_missing_document_refuses_by_name() {
    let cases = [
        (
            RegistrationEvidence {
                battery_electric_only: false,
                ..RegistrationEvidence::complete()
            },
            "reines Batterieelektrofahrzeug",
        ),
        (
            RegistrationEvidence {
                certificate_on_file: false,
                ..RegistrationEvidence::complete()
            },
            "Zulassungsbescheinigung Teil I is on file",
        ),
        (
            RegistrationEvidence {
                certificate_current: false,
                ..RegistrationEvidence::complete()
            },
            "not the current one",
        ),
    ];
    for (evidence, expected) in cases {
        let mut claim = builder(2027);
        let refused = claim.vehicle(
            "bus-1",
            "DEPOT",
            VehicleClass::HeavyM3OrN3,
            evidence,
            &bus_estimate(),
        );
        let message = refused.expect_err("the paragraph refuses this").to_string();
        assert!(message.contains(expected), "{message}");
    }
}

/// A *Schätzwert* is published for a class, and counting a bus at a car's
/// estimate invents a quantity nobody announced `[38k §7(3)]`.
#[test]
fn an_estimate_belongs_to_the_class_it_was_announced_for() {
    let mut claim = builder(2027);
    let refused = claim.vehicle(
        "bus-1",
        "DEPOT",
        VehicleClass::HeavyM3OrN3,
        RegistrationEvidence::complete(),
        &car_estimate(),
    );
    assert!(
        matches!(refused, Err(ThgError::NoEstimateForClass { .. })),
        "{refused:?}"
    );
}

/// A third party files only for the keepers that designated it `[38k §5(2)]` —
/// and under `[38k §7(1) S. 2]` the keeper *is* the Ladepunktbetreiber.
#[test]
fn a_filer_counts_only_the_keepers_that_designated_it() {
    let mut claim = EstimateClaimBuilder::new(
        2027,
        Attribution::designated("QUOTA-HAUS", ["DEPOT"]),
        basis(),
        DriveEfficiency::BatteryElectric,
    )
    .expect("a countable year");

    claim
        .vehicle(
            "bus-1",
            "DEPOT",
            VehicleClass::HeavyM3OrN3,
            RegistrationEvidence::complete(),
            &bus_estimate(),
        )
        .expect("designated for this keeper");

    let refused = claim.vehicle(
        "bus-2",
        "SOMEBODY-ELSE",
        VehicleClass::HeavyM3OrN3,
        RegistrationEvidence::complete(),
        &bus_estimate(),
    );
    assert!(
        matches!(refused, Err(ThgError::NoAgreement { .. })),
        "{refused:?}"
    );
}

/// A heavy vehicle counted before 2027 counts at the ordinary factor, and the
/// notification says so rather than leaving an operator to find out.
#[test]
fn a_bus_before_2027_is_counted_like_anything_else_and_the_file_says_so() {
    let mut claim = builder(2026);
    claim
        .vehicle(
            "bus-1",
            "DEPOT",
            VehicleClass::HeavyM3OrN3,
            RegistrationEvidence::complete(),
            &bus_estimate(),
        )
        .expect("a countable bus");

    let filed = claim.build().expect("a notification");
    assert_eq!(filed.value.counted_megawatt_hours(), dec("216"), "72 × 3");
    assert!(
        filed
            .reasons()
            .any(|reason| reason.contains("2027") && reason.contains("§5(3)")),
        "{:?}",
        filed.reasons().collect::<Vec<_>>()
    );

    // …and the same bus, one year later.
    let mut next = builder(2027);
    next.vehicle(
        "bus-1",
        "DEPOT",
        VehicleClass::HeavyM3OrN3,
        RegistrationEvidence::complete(),
        &bus_estimate(),
    )
    .expect("a countable bus");
    let filed = next.build().expect("a notification");
    assert_eq!(filed.value.counted_megawatt_hours(), dec("288"), "72 × 4");
    assert!(
        filed.is_lossless(),
        "{:?}",
        filed.reasons().collect::<Vec<_>>()
    );
}

/// The two routes have two deadlines, and one of them is not in the year the
/// other one is.
#[test]
fn the_route_a_claim_is_filed_under_decides_its_deadline() {
    assert_eq!(
        Route::PublicChargePoints.deadline(2027),
        date!(2028 - 02 - 28)
    );
    assert_eq!(
        Route::EstimatedPerVehicle.deadline(2027),
        date!(2027 - 11 - 15)
    );
}

#[test]
fn a_published_estimate_arrives_through_its_own_constructor() {
    // `Estimate::announced` refuses a negative, because the figure multiplies
    // into `[38k §5(3)]`'s reference value and out the other side as an
    // emissions saving. A derived `Deserialize` restored it from a store
    // without asking, which is the one path a value in a filing arrives by
    // (D264).
    let announced =
        Estimate::announced(dec("2000"), VehicleClass::Other, "BAnz AT 31.10.2026 B5").unwrap();
    let json = serde_json::to_string(&announced).expect("a published estimate serialises");
    assert_eq!(
        serde_json::from_str::<Estimate>(&json).expect("and reads back"),
        announced
    );

    let negative = json.replace("2000", "-2000");
    assert!(
        serde_json::from_str::<Estimate>(&negative).is_err(),
        "a negative published estimate is not one"
    );
}
