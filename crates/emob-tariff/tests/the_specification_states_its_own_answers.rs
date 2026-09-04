//! `[OCPI 2.3.0]`'s own worked examples, with the totals it publishes.
//!
//! Every other test in this crate asserts what *this* engine does. These assert
//! what the specification says the answer is — the tariffs are the ones in
//! `mod_tariffs.asciidoc`'s `examples/` directory, the sessions are the ones its
//! prose walks through, and the expected figures are the ones printed in its own
//! breakdown tables. A change that keeps every internal test green and moves one
//! of these has changed what emob charges a driver relative to the document two
//! companies settle against.
//!
//! The rounding rule is the one they exist for. `step_size` is applied **once
//! per session** for `ENERGY` and **once for `TIME` and `PARKING_TIME`
//! combined** `[OCPI 2.3.0 §mod_cdrs_step_size]` — never once per price
//! component, which is the reading that looks right, passes every unit test
//! written against itself, and over-charges every tiered tariff.

use emob_core::{Currency, Energy, TimeZone};
use emob_tariff::{
    Chargeable, Dimension, Period, PriceComponent, PriceLimit, Rated, Restrictions, Tariff,
    TariffElement, TariffKind, TaxIncluded, rate,
};
use rust_decimal::Decimal;
use std::str::FromStr;
use time::macros::{datetime, time};

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).expect("a decimal literal")
}

fn tariff(elements: Vec<TariffElement>) -> Tariff {
    Tariff {
        id: "22".parse().expect("a tariff id"),
        currency: Currency::EUR,
        kind: TariffKind::AdHoc,
        // The examples are written in local wall-clock terms and carry no zone;
        // the sessions below are stamped in the same one.
        time_zone: TimeZone::new("Europe/Berlin").expect("a zone"),
        tax_included: TaxIncluded::No,
        elements,
        min_price: None,
        max_price: None,
        valid_from: None,
        valid_until: None,
    }
}

/// The line for one dimension at one price, as the specification's breakdown
/// tables state them.
fn line(rated: &Rated, dimension: Dimension, price: &str) -> Option<(Decimal, Decimal)> {
    rated
        .lines
        .iter()
        .find(|l| l.dimension == dimension && l.unit_price == dec(price))
        .map(|l| (l.base_quantity, l.amount))
}

/// `examples/tariff_14_step_size.json` — the tariff all three time examples use.
///
/// € 1.20/h charging before 17:00 in 30-minute blocks, € 2.40/h after in
/// 15-minute blocks, € 1.00/h parking before 20:00 in 15-minute blocks and no
/// parking price at all after it.
fn step_size_tariff() -> Tariff {
    tariff(vec![
        TariffElement {
            components: vec![
                PriceComponent::new(Dimension::Time, dec("1.20")).with_step_size(1800),
                PriceComponent::new(Dimension::ParkingTime, dec("1.00")).with_step_size(900),
            ],
            restrictions: Restrictions {
                start_time: Some(time!(00:00)),
                end_time: Some(time!(17:00)),
                ..Restrictions::default()
            },
        },
        TariffElement {
            components: vec![
                PriceComponent::new(Dimension::Time, dec("2.40")).with_step_size(900),
                PriceComponent::new(Dimension::ParkingTime, dec("1.00")).with_step_size(900),
            ],
            restrictions: Restrictions {
                start_time: Some(time!(17:00)),
                end_time: Some(time!(20:00)),
                ..Restrictions::default()
            },
        },
        TariffElement {
            components: vec![PriceComponent::new(Dimension::Time, dec("2.40")).with_step_size(900)],
            restrictions: Restrictions {
                start_time: Some(time!(20:00)),
                // "To stop at end of the day use: 00:00."
                end_time: Some(time!(00:00)),
                ..Restrictions::default()
            },
        },
    ])
}

/// `[OCPI 2.3.0 §mod_tariffs]` "Example: switching to different Tariff
/// Element #1".
///
/// > An EV driver plugs in at 16:55 and charges for 10 minutes (`TIME`). They
/// > then stop charging but stay plugged in for 2 more minutes
/// > (`PARKING_TIME`). … the session costs € 0.55 ex VAT.
///
/// | Dimension | Quantity | Price | Cost |
/// |---|---|---|---|
/// | Charging time | 5 minutes | 1.20 per hour | 0.10 |
/// | Charging time | 5 minutes | 2.40 per hour | 0.20 |
/// | Time | 15 minutes | 1.00 per hour | 0.25 |
#[test]
fn switching_tariff_element_while_charging_then_parking() {
    let session = Chargeable::new(vec![
        Period::charging(
            datetime!(2026-01-02 16:55 +1),
            datetime!(2026-01-02 17:05 +1),
            Energy::from_kwh(dec("5.0")).expect("energy"),
        ),
        Period::parked(
            datetime!(2026-01-02 17:05 +1),
            datetime!(2026-01-02 17:07 +1),
        ),
    ])
    .expect("a session");

    let rated = rate(&step_size_tariff(), &session);

    assert_eq!(
        line(&rated, Dimension::Time, "1.20"),
        Some((dec("300"), dec("0.10"))),
        "five minutes before 17:00 at the 1.20 rate"
    );
    assert_eq!(
        line(&rated, Dimension::Time, "2.40"),
        Some((dec("300"), dec("0.20"))),
        "five minutes after 17:00 at the 2.40 rate, and not rounded: the \
         charging time is followed by a parking period"
    );
    assert_eq!(
        line(&rated, Dimension::ParkingTime, "1.00"),
        Some((dec("900"), dec("0.25"))),
        "two minutes of parking, in the 15-minute block the session ended on"
    );
    assert_eq!(rated.total().to_string(), "0.55 EUR");
}

/// `[OCPI 2.3.0 §mod_tariffs]` "Example: switching to different Tariff
/// Element #2".
///
/// > An EV driver plugs in at 16:35 and charges for 35 minutes … the total
/// > charging time is rounded up from 35 to 45 minutes. When considering the
/// > already billed 25 minutes of charging time before 17:00, we are left with
/// > 20 minutes to bill after 17:00. That leads to a session fee of € 1.30.
///
/// The whole of the rule is in that sentence: the block applies to the
/// session's **total**, with the last component's step size, and the surplus is
/// billed at the last component's **price** rather than spread across the tiers.
#[test]
fn the_block_rounds_the_session_total_and_the_surplus_takes_the_last_price() {
    let session = Chargeable::new(vec![Period::charging(
        datetime!(2026-01-02 16:35 +1),
        datetime!(2026-01-02 17:10 +1),
        Energy::from_kwh(dec("10.0")).expect("energy"),
    )])
    .expect("a session");

    let rated = rate(&step_size_tariff(), &session);

    assert_eq!(
        line(&rated, Dimension::Time, "1.20"),
        Some((dec("1500"), dec("0.50"))),
        "twenty-five minutes before 17:00, untouched by the block"
    );
    assert_eq!(
        line(&rated, Dimension::Time, "2.40"),
        Some((dec("1200"), dec("0.80"))),
        "ten minutes charged after 17:00 plus the ten the block added — at the \
         last component's price, which is what makes this 1.30 and not 1.50"
    );
    assert_eq!(rated.total().to_string(), "1.30 EUR");
}

/// `[OCPI 2.3.0 §mod_tariffs]` "Example: switching to Free-of-Charge Tariff
/// Element".
///
/// > An EV driver plugs in at 19:40 and charges for 12 minutes (`TIME`). They
/// > then stop charging but stay plugged in for 20 more minutes. … The total of
/// > billable parking time for the session is 8 minutes. This is rounded up to
/// > 15 minutes … So the user is billed € 0.25 for 15 minutes of parking and
/// > that makes a total session fee of € 0.73.
///
/// The twelve minutes of parking after 20:00 are priced by nothing, which is
/// what "free of charge" means here — and they are *reported* as unpriced
/// rather than silently absent, because a quantity nobody priced is the thing a
/// dispute is about.
#[test]
fn parking_that_becomes_free_still_rounds_the_part_that_was_not() {
    let session = Chargeable::new(vec![
        Period::charging(
            datetime!(2026-01-02 19:40 +1),
            datetime!(2026-01-02 19:52 +1),
            Energy::from_kwh(dec("6.0")).expect("energy"),
        ),
        Period::parked(
            datetime!(2026-01-02 19:52 +1),
            datetime!(2026-01-02 20:12 +1),
        ),
    ])
    .expect("a session");

    let rated = rate(&step_size_tariff(), &session);

    assert_eq!(
        line(&rated, Dimension::Time, "2.40"),
        Some((dec("720"), dec("0.48"))),
        "twelve minutes of charging, not rounded: a parking period follows it"
    );
    assert_eq!(
        line(&rated, Dimension::ParkingTime, "1.00"),
        Some((dec("900"), dec("0.25"))),
        "eight billable minutes of parking, in a 15-minute block"
    );
    assert_eq!(rated.total().to_string(), "0.73 EUR");
    assert!(
        rated
            .reasons()
            .any(|r| r.contains("ParkingTime") || r.contains("PARKING_TIME")),
        "the twelve free minutes are named rather than dropped: {:?}",
        rated.reasons().collect::<Vec<_>>()
    );
}

/// `[OCPI 2.3.0 §mod_cdrs_step_size]`, the energy paragraph.
///
/// > Energy costs € 0.20 per kWh before 17:00 and € 0.27 per kWh after 17:00.
/// > Both Price Components have a `step_size` of 500 Wh. If a driver charges
/// > 4.3 kWh before 17:00 and 1.1 kWh after 17:00, a total of 5.4 kWh is
/// > charged. The `step_size` rounds this up to 5.5 kWh total. **It does NOT
/// > round the energy used after 17:00 to 1.5 kWh.**
#[test]
fn an_energy_block_rounds_the_session_total_once() {
    let t = tariff(vec![
        TariffElement {
            components: vec![
                PriceComponent::new(Dimension::Energy, dec("0.20")).with_step_size(500),
            ],
            restrictions: Restrictions {
                end_time: Some(time!(17:00)),
                ..Restrictions::default()
            },
        },
        TariffElement {
            components: vec![
                PriceComponent::new(Dimension::Energy, dec("0.27")).with_step_size(500),
            ],
            restrictions: Restrictions::default(),
        },
    ]);
    let session = Chargeable::new(vec![
        Period::charging(
            datetime!(2026-01-02 16:00 +1),
            datetime!(2026-01-02 17:00 +1),
            Energy::from_kwh(dec("4.3")).expect("energy"),
        ),
        Period::charging(
            datetime!(2026-01-02 17:00 +1),
            datetime!(2026-01-02 18:00 +1),
            Energy::from_kwh(dec("1.1")).expect("energy"),
        ),
    ])
    .expect("a session");

    let rated = rate(&t, &session);

    assert_eq!(
        line(&rated, Dimension::Energy, "0.20"),
        Some((dec("4.3"), dec("0.860"))),
        "the first tier is not rounded"
    );
    assert_eq!(
        line(&rated, Dimension::Energy, "0.27"),
        Some((dec("1.2"), dec("0.324"))),
        "1.1 kWh plus the 0.1 the session-wide block added — not 1.5 kWh"
    );
    // 4.3 × 0.20 + 1.2 × 0.27 = 1.184.
    assert_eq!(rated.total().to_string(), "1.18 EUR");
}

/// `[OCPI 2.3.0 §mod_cdrs_step_size]`, the time-then-parking paragraph.
///
/// > `step_size` for both charging (`TIME`) and parking is 5 minutes. After 21
/// > minutes of charging, the EV is full but remains connected for 7 more
/// > minutes. The cost of charging will be calculated based on 21 minutes (not
/// > 25). The cost of parking will be calculated based on 10 minutes.
#[test]
fn the_time_family_rounds_only_the_dimension_the_session_ended_on() {
    let t = tariff(vec![TariffElement {
        components: vec![
            PriceComponent::new(Dimension::Time, dec("1.00")).with_step_size(300),
            PriceComponent::new(Dimension::ParkingTime, dec("2.00")).with_step_size(300),
        ],
        restrictions: Restrictions::default(),
    }]);
    let session = Chargeable::new(vec![
        Period::charging(
            datetime!(2026-01-02 10:00 +1),
            datetime!(2026-01-02 10:21 +1),
            Energy::from_kwh(dec("5.0")).expect("energy"),
        ),
        Period::parked(
            datetime!(2026-01-02 10:21 +1),
            datetime!(2026-01-02 10:28 +1),
        ),
    ])
    .expect("a session");

    let rated = rate(&t, &session);

    assert_eq!(
        line(&rated, Dimension::Time, "1.00").map(|(q, _)| q),
        Some(dec("1260")),
        "21 minutes, not 25: the charging time is followed by another \
         time-based period"
    );
    assert_eq!(
        line(&rated, Dimension::ParkingTime, "2.00").map(|(q, _)| q),
        Some(dec("600")),
        "7 minutes of parking in a 5-minute block is 10"
    );
}

/// `examples/tariffrestriction_example_max_power.json`, with the session the
/// prose beside it walks through.
///
/// > For a charging session where the EV charges the first kWh with a power of
/// > 6 kW, increases the power to 48 kW for the next 40 kWh and reduces it
/// > again to 4 kW after that for another 0.5 kWh … this tariff will result in
/// > costs of € 20.30 (excl. VAT).
#[test]
fn a_power_restriction_selects_the_band_per_period() {
    let t = tariff(vec![
        TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.20"))],
            restrictions: Restrictions {
                max_power_kw: Some(dec("16.00")),
                ..Restrictions::default()
            },
        },
        TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.35"))],
            restrictions: Restrictions {
                max_power_kw: Some(dec("32.00")),
                ..Restrictions::default()
            },
        },
        TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.50"))],
            restrictions: Restrictions::default(),
        },
    ]);
    // 1 kWh at 6 kW is ten minutes; 40 kWh at 48 kW is fifty; 0.5 kWh at 4 kW
    // is seven and a half.
    let session = Chargeable::new(vec![
        Period::charging(
            datetime!(2026-01-02 10:00 +1),
            datetime!(2026-01-02 10:10 +1),
            Energy::from_kwh(dec("1.0")).expect("energy"),
        ),
        Period::charging(
            datetime!(2026-01-02 10:10 +1),
            datetime!(2026-01-02 11:00 +1),
            Energy::from_kwh(dec("40.0")).expect("energy"),
        ),
        Period::charging(
            datetime!(2026-01-02 11:00 +1),
            datetime!(2026-01-02 11:07:30 +1),
            Energy::from_kwh(dec("0.5")).expect("energy"),
        ),
    ])
    .expect("a session");

    let rated = rate(&t, &session);

    assert_eq!(
        line(&rated, Dimension::Energy, "0.20").map(|(q, _)| q),
        Some(dec("1.5")),
        "the 6 kW and the 4 kW periods are both in the sub-16 kW band"
    );
    assert_eq!(
        line(&rated, Dimension::Energy, "0.50").map(|(q, _)| q),
        Some(dec("40.0")),
        "48 kW falls through both bands to the unrestricted element"
    );
    assert_eq!(rated.total().to_string(), "20.30 EUR");
}

/// `[OCPI 2.3.0 §mod_tariffs_tariffrestrictions_class]`: "To stop at end of the
/// day use: 00:00."
///
/// Read as the exclusive instant the field otherwise is, a `00:00` end closes
/// the window before it opens: the element matches nothing, prices nothing, and
/// leaves the whole session unpriced — silently, on a tariff whose author wrote
/// exactly what the specification told them to write.
#[test]
fn an_end_time_of_midnight_means_the_end_of_the_day() {
    let t = tariff(vec![TariffElement {
        components: vec![PriceComponent::new(Dimension::Energy, dec("0.50"))],
        restrictions: Restrictions {
            end_time: Some(time!(00:00)),
            ..Restrictions::default()
        },
    }]);
    let session = Chargeable::energy_only(
        Energy::from_kwh(dec("10")).expect("energy"),
        datetime!(2026-01-02 10:00 +1),
        datetime!(2026-01-02 11:00 +1),
    )
    .expect("a session");

    let rated = rate(&t, &session);
    assert_eq!(rated.total().to_string(), "5.00 EUR");
    assert_eq!(rated.notes.len(), 0, "{:?}", rated.notes);
}

/// `[OCPI 2.3.0 §mod_cdrs_chargingperiod_class]`, the erratum.
///
/// > Earlier versions of the OCPI 2.3.0 specification document **mistakenly**
/// > defined `PARKING_TIME` as "Time during this ChargingPeriod not charging".
/// > Under that definition, drivers would be exposed to penalizing loitering
/// > fees not only when they leave their vehicle in a charging session after it
/// > has been fully charged, **but also when the EVSE is not offering energy to
/// > the vehicle while the vehicle is still requesting power**.
///
/// Two sessions, identical in every measurable respect — same duration, same
/// energy, same tariff, twenty minutes of it with nothing flowing. In one the
/// **car** stopped asking; in the other the **point** stopped offering. Under
/// the old boolean they cost the same, and one of them should not.
#[test]
fn a_point_withholding_power_owes_no_occupancy_fee() {
    let t = tariff(vec![TariffElement {
        components: vec![
            PriceComponent::new(Dimension::Energy, dec("0.50")),
            PriceComponent::new(Dimension::Time, dec("0.00")),
            PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
        ],
        restrictions: Restrictions::default(),
    }]);

    let charge = Period::charging(
        datetime!(2026-01-02 10:00 +1),
        datetime!(2026-01-02 10:40 +1),
        Energy::from_kwh(dec("20.0")).expect("energy"),
    );
    let idle = (
        datetime!(2026-01-02 10:40 +1),
        datetime!(2026-01-02 11:00 +1),
    );

    let full_battery = rate(
        &t,
        &Chargeable::new(vec![charge.clone(), Period::parked(idle.0, idle.1)]).expect("a session"),
    );
    let curtailed = rate(
        &t,
        &Chargeable::new(vec![charge, Period::withheld(idle.0, idle.1)]).expect("a session"),
    );

    // The car stopped asking: twenty minutes of occupancy at €6.00 an hour.
    assert_eq!(full_battery.total().to_string(), "12.00 EUR");
    assert_eq!(
        line(&full_battery, Dimension::ParkingTime, "6.00").map(|(q, _)| q),
        Some(dec("1200"))
    );

    // The point stopped offering: the energy is the same and the fee is gone.
    assert_eq!(curtailed.total().to_string(), "10.00 EUR");
    assert_eq!(line(&curtailed, Dimension::ParkingTime, "6.00"), None);
    assert_eq!(
        line(&curtailed, Dimension::Time, "0.00").map(|(q, _)| q),
        Some(dec("2400")),
        "and it is not quietly moved to the charging-time dimension either: \
         the charging time is the forty minutes that charged"
    );

    // The minutes are accounted for rather than lost.
    assert!(
        curtailed
            .reasons()
            .any(|r| r.contains("1200 s") && r.contains("neither")),
        "{:?}",
        curtailed.reasons().collect::<Vec<_>>()
    );
}

/// `examples/tariff_6_025kwh_start_max_price.json`, and the NOTE beside
/// `max_price` in `[OCPI 2.3.0 §mod_tariffs_tariff_object]`.
///
/// > As the taxes on a Charging Session might be different for different parts
/// > of the Session, there might be situations where the maximum cost after
/// > taxes is reached earlier or later than the maximum price before taxes. So
/// > as a rule, **they both apply**.
///
/// The example is exactly such a session: a session fee at 20 % beside energy
/// at 10 %, under a € 10 net / € 11 gross ceiling. The two are not proportional,
/// so a stack that keeps one figure and derives the other lets the total past
/// whichever ceiling it did not keep — here five cents past a maximum the
/// operator published to the driver.
#[test]
fn both_limbs_of_a_price_limit_bind() {
    let t = Tariff {
        id: "6".parse().expect("a tariff id"),
        currency: Currency::EUR,
        kind: TariffKind::AdHoc,
        time_zone: TimeZone::new("Europe/Berlin").expect("a zone"),
        tax_included: TaxIncluded::No,
        elements: vec![TariffElement::unrestricted(vec![
            PriceComponent::new(Dimension::Flat, dec("0.50")).with_vat(dec("20")),
            PriceComponent::new(Dimension::Energy, dec("0.25")).with_vat(dec("10")),
        ])],
        min_price: None,
        max_price: Some(PriceLimit::net_and_gross(dec("10.00"), dec("11.00"))),
        valid_from: None,
        valid_until: None,
    };
    let session = |kwh: &str| {
        Chargeable::energy_only(
            Energy::from_kwh(dec(kwh)).expect("energy"),
            datetime!(2026-01-02 10:00 +1),
            datetime!(2026-01-02 11:00 +1),
        )
        .expect("a session")
    };

    // "For a charging session where 50 kWh are charged, this tariff will result
    // in costs of € 10.00 (excl. VAT) or € 11.00 (incl. VAT) due to the price
    // limit." Both ceilings hold, and the gross one is the tighter of the two:
    // € 10.00 net at this mix of rates would gross up to € 11.05.
    let capped = rate(&t, &session("50"));
    assert_eq!(capped.gross().to_string(), "11.00 EUR");
    assert!(
        capped.total().amount() <= dec("10.00"),
        "the net ceiling holds too: {}",
        capped.total()
    );

    // "If only 30 kWh were charged, the costs would be € 8.00 (excl. VAT) and
    // € 8.85 (incl. VAT)" — neither ceiling binds, so nothing moves.
    let under = rate(&t, &session("30"));
    assert_eq!(under.total().to_string(), "8.00 EUR");
    assert_eq!(under.gross().to_string(), "8.85 EUR");
    assert!(under.lines_sum_to_total());
}

/// `examples/tariffrestriction_example_max_duration.json` — the supermarket.
///
/// > First 30 minutes of charging is free. Charging fee of € 0.25 per kWh
/// > (excl. VAT) after 30 minutes. Charging fee of € 0.40 per kWh (excl. VAT)
/// > after 60 minutes. For a charging session with a duration of 40 minutes
/// > where 5 kWh are charged during the first 30 minutes and another 1.2 kWh in
/// > the remaining 10 minutes of the session, this tariff will result in costs
/// > of € 0.30 (excl. VAT).
#[test]
fn a_duration_restriction_selects_the_band_the_session_has_reached() {
    let band = |price: &str, max: Option<u64>| TariffElement {
        components: vec![PriceComponent::new(Dimension::Energy, dec(price)).with_vat(dec("20"))],
        restrictions: Restrictions {
            max_duration_s: max,
            ..Restrictions::default()
        },
    };
    let t = tariff(vec![
        band("0.00", Some(1800)),
        band("0.25", Some(3600)),
        band("0.40", None),
    ]);
    let session = Chargeable::new(vec![
        Period::charging(
            datetime!(2026-01-02 10:00 +1),
            datetime!(2026-01-02 10:30 +1),
            Energy::from_kwh(dec("5.0")).expect("energy"),
        ),
        Period::charging(
            datetime!(2026-01-02 10:30 +1),
            datetime!(2026-01-02 10:40 +1),
            Energy::from_kwh(dec("1.2")).expect("energy"),
        ),
    ])
    .expect("a session");

    let rated = rate(&t, &session);
    assert_eq!(
        line(&rated, Dimension::Energy, "0.25").map(|(q, _)| q),
        Some(dec("1.2")),
        "the session has passed thirty minutes and not sixty"
    );
    assert_eq!(rated.total().to_string(), "0.30 EUR");
}

/// `examples/tariff_13_simple_3hour_5parking.json`.
///
/// > A charging session of 2.5 hours (charging), where the vehicle is parked for
/// > 42 more minutes after charging ended … will result in total cost of
/// > € 11.25 (excl. VAT) or € 12.75 (incl. VAT). Because the parking time is
/// > billed per 5 minutes, the driver has to pay for 45 minutes of parking even
/// > though they left 42 minutes after their vehicle stopped charging.
///
/// Two VAT rates under one session, and the block on the dimension the session
/// ended on — the two rules that have to hold together for the gross figure to
/// come out.
#[test]
fn charging_time_and_parking_at_two_rates() {
    let t = tariff(vec![TariffElement::unrestricted(vec![
        PriceComponent::new(Dimension::Time, dec("3.00"))
            .with_vat(dec("10"))
            .with_step_size(60),
        PriceComponent::new(Dimension::ParkingTime, dec("5.00"))
            .with_vat(dec("20"))
            .with_step_size(300),
    ])]);
    let session = Chargeable::new(vec![
        Period::charging(
            datetime!(2026-01-02 10:00 +1),
            datetime!(2026-01-02 12:30 +1),
            Energy::from_kwh(dec("40.0")).expect("energy"),
        ),
        Period::parked(
            datetime!(2026-01-02 12:30 +1),
            datetime!(2026-01-02 13:12 +1),
        ),
    ])
    .expect("a session");

    let rated = rate(&t, &session);
    assert_eq!(
        line(&rated, Dimension::Time, "3.00").map(|(q, _)| q),
        Some(dec("9000")),
        "two and a half hours of charging, not rounded"
    );
    assert_eq!(
        line(&rated, Dimension::ParkingTime, "5.00").map(|(q, _)| q),
        Some(dec("2700")),
        "forty-two minutes of parking in a five-minute block is forty-five"
    );
    assert_eq!(rated.total().to_string(), "11.25 EUR");
    assert_eq!(rated.gross().to_string(), "12.75 EUR");
}

/// `examples/tariff_10_025kwh_parking_start.json`.
///
/// > For a charging session where 20 kWh are charged and the vehicle is parked
/// > for 40 minutes after the session ended, this tariff will result in costs of
/// > € 7.00 (excl. VAT) or € 7.90 (incl. VAT).
///
/// Three dimensions at two rates, and the flat fee charged once.
#[test]
fn a_start_fee_energy_and_parking_at_two_rates() {
    let t = tariff(vec![TariffElement::unrestricted(vec![
        PriceComponent::new(Dimension::Flat, dec("0.50")).with_vat(dec("20")),
        PriceComponent::new(Dimension::Energy, dec("0.25")).with_vat(dec("10")),
        PriceComponent::new(Dimension::ParkingTime, dec("2.00"))
            .with_vat(dec("20"))
            .with_step_size(900),
    ])]);
    let session = Chargeable::new(vec![
        Period::charging(
            datetime!(2026-01-02 10:00 +1),
            datetime!(2026-01-02 11:00 +1),
            Energy::from_kwh(dec("20.0")).expect("energy"),
        ),
        Period::parked(
            datetime!(2026-01-02 11:00 +1),
            datetime!(2026-01-02 11:40 +1),
        ),
    ])
    .expect("a session");

    let rated = rate(&t, &session);
    assert_eq!(
        line(&rated, Dimension::ParkingTime, "2.00").map(|(q, _)| q),
        Some(dec("2700")),
        "forty minutes in a fifteen-minute block is forty-five"
    );
    assert_eq!(
        line(&rated, Dimension::Flat, "0.50").map(|(q, _)| q),
        Some(dec("1")),
        "one session fee, however many periods"
    );
    assert_eq!(rated.total().to_string(), "7.00 EUR");
    assert_eq!(rated.gross().to_string(), "7.90 EUR");
}

/// `examples/tariff_12_025kwh_min_price.json`.
///
/// > This tariff will result in costs of € 5.00 (excl. VAT) or € 5.50 (incl.
/// > VAT) when 20 kWh are charged. But if less than 2 kWh is charged, € 0.50
/// > (excl. VAT) or € 0.55 (incl. VAT) will be billed.
///
/// The `min_price` here states **both** limbs, and at one VAT rate they agree —
/// which is exactly why a stack that keeps one figure passes this example and
/// fails the `max_price` one two sections later.
#[test]
fn a_minimum_price_states_both_limbs_and_at_one_rate_they_agree() {
    let mut t = tariff(vec![TariffElement::unrestricted(vec![
        PriceComponent::new(Dimension::Energy, dec("0.25")).with_vat(dec("10")),
    ])]);
    t.min_price = Some(PriceLimit::net_and_gross(dec("0.50"), dec("0.55")));
    let session = |kwh: &str| {
        Chargeable::energy_only(
            Energy::from_kwh(dec(kwh)).expect("energy"),
            datetime!(2026-01-02 10:00 +1),
            datetime!(2026-01-02 11:00 +1),
        )
        .expect("a session")
    };

    let ordinary = rate(&t, &session("20"));
    assert_eq!(ordinary.total().to_string(), "5.00 EUR");
    assert_eq!(ordinary.gross().to_string(), "5.50 EUR");
    assert!(ordinary.lines_sum_to_total(), "the minimum does not bind");

    // "if less than 2 kWh is charged" — 1 kWh comes to €0.25 and is lifted.
    let small = rate(&t, &session("1"));
    assert_eq!(small.total().to_string(), "0.50 EUR");
    assert_eq!(small.gross().to_string(), "0.55 EUR");
}

/// `examples/tariff_2_alt_text.json` — the rounding, at a rate with a decimal in
/// it.
///
/// > € 1.90 per hour (excl. VAT), 5.2% VAT, billed per 5 minutes. For a charging
/// > session of 2.5 hours, this tariff will result in costs of € 4.75 (excl.
/// > VAT) or € 5.00 (incl. VAT).
///
/// `4.75 × 1.052` is `4.9970`, and the published figure is `5.00`: the gross is
/// rounded to the minor unit **once**, at the tax category, and not carried
/// through at four decimals.
#[test]
fn a_fractional_vat_rate_rounds_at_the_category() {
    let t = tariff(vec![TariffElement::unrestricted(vec![
        PriceComponent::new(Dimension::Time, dec("1.90"))
            .with_vat(dec("5.2"))
            .with_step_size(300),
    ])]);
    let session = Chargeable::new(vec![Period::charging(
        datetime!(2026-01-02 10:00 +1),
        datetime!(2026-01-02 12:30 +1),
        Energy::from_kwh(dec("40.0")).expect("energy"),
    )])
    .expect("a session");

    let rated = rate(&t, &session);
    assert_eq!(rated.total().to_string(), "4.75 EUR");
    assert_eq!(rated.gross().to_string(), "5.00 EUR");
}

// ── Reservations ────────────────────────────────────────────────────────────
//
// `[OCPI 2.3.0]` prices a reservation through a restriction rather than a
// dimension, over a window that has already run before any session begins. Four
// worked examples, each with the breakdown table the specification prints.

use emob_tariff::{Reservation, ReservationRestriction, rate_reservation};

/// One element restricted to a reservation outcome.
fn reserved(kind: ReservationRestriction, components: Vec<PriceComponent>) -> TariffElement {
    TariffElement {
        components,
        restrictions: Restrictions {
            reservation: Some(kind),
            ..Restrictions::default()
        },
    }
}

/// The session leg every reservation example shares: € 0.50 flat at 20 %,
/// € 0.25 per kWh at 10 %.
fn session_leg() -> TariffElement {
    TariffElement::unrestricted(vec![
        PriceComponent::new(Dimension::Flat, dec("0.50")).with_vat(dec("20")),
        PriceComponent::new(Dimension::Energy, dec("0.25")).with_vat(dec("10")),
    ])
}

fn twenty_kwh() -> Chargeable {
    Chargeable::energy_only(
        Energy::from_kwh(dec("20.0")).expect("energy"),
        datetime!(2026-01-02 10:15 +1),
        datetime!(2026-01-02 11:15 +1),
    )
    .expect("a session")
}

/// `examples/tariff_15_reservation_5_euro_per_hour.json`.
///
/// > For a charging session that was started 15 minutes after the reservation
/// > time, where the driver charges 20 kWh, this tariff will result in costs of
/// > € 6.75 (excl. VAT) or € 7.60 (incl. VAT).
///
/// | Dimension | Quantity | Price ex VAT | Cost ex VAT | VAT |
/// |---|---|---|---|---|
/// | Flat | 1 | 0.50 | 0.50 | 20 % |
/// | Energy | 20 kWh | 0.25 per kWh | 5.00 | 10 % |
/// | Reservation | 15 minutes | 5.00 per hour | 1.25 | 20 % |
#[test]
fn a_reservation_is_priced_beside_the_session_it_led_to() {
    let t = tariff(vec![
        reserved(
            ReservationRestriction::Reservation,
            vec![
                PriceComponent::new(Dimension::Time, dec("5.00"))
                    .with_vat(dec("20"))
                    .with_step_size(60),
            ],
        ),
        session_leg(),
    ]);

    let held = Reservation::honoured(
        datetime!(2026-01-02 10:00 +1),
        datetime!(2026-01-02 10:15 +1),
    );
    let reservation = rate_reservation(&t, &held);
    assert_eq!(reservation.total().to_string(), "1.25 EUR");
    assert_eq!(reservation.gross().to_string(), "1.50 EUR");

    // The session is rated by the unrestricted element, and the reservation
    // element does not reach it — which is the whole point of the split.
    let session = rate(&t, &twenty_kwh());
    assert_eq!(session.total().to_string(), "5.50 EUR");
    assert_eq!(
        session.amount_for(Dimension::Time),
        None,
        "the reservation's minutes are not the session's charging time"
    );

    // 0.50 + 5.00 + 1.25, and 0.60 + 5.50 + 1.50.
    assert_eq!(
        session.total().amount() + reservation.total().amount(),
        dec("6.75")
    );
    assert_eq!(
        session.gross().amount() + reservation.gross().amount(),
        dec("7.60")
    );
}

/// `examples/tariff_16_reservation_2_euro_fee_5_euro_per_hour.json`.
///
/// > For a charging session that was started 13 minutes after the reservation
/// > time … € 8.75 (excl. VAT) or € 10.00 (incl. VAT). Because the reservation
/// > fee is billed per 5 minutes, the driver has to pay for 15 minutes of
/// > reservation even though they started the charging session 13 minutes after
/// > the reservation time.
#[test]
fn a_reservation_fee_and_a_reservation_rate_are_two_lines() {
    let t = tariff(vec![
        reserved(
            ReservationRestriction::Reservation,
            vec![
                PriceComponent::new(Dimension::Flat, dec("2.00")).with_vat(dec("20")),
                PriceComponent::new(Dimension::Time, dec("5.00"))
                    .with_vat(dec("20"))
                    .with_step_size(300),
            ],
        ),
        session_leg(),
    ]);

    let held = Reservation::honoured(
        datetime!(2026-01-02 10:00 +1),
        datetime!(2026-01-02 10:13 +1),
    );
    let reservation = rate_reservation(&t, &held);
    assert_eq!(
        line(&reservation, Dimension::Time, "5.00").map(|(q, _)| q),
        Some(dec("900")),
        "thirteen minutes in a five-minute block is fifteen"
    );
    assert_eq!(
        line(&reservation, Dimension::Flat, "2.00").map(|(q, _)| q),
        Some(dec("1"))
    );
    assert_eq!(reservation.total().to_string(), "3.25 EUR");

    let session = rate(&t, &twenty_kwh());
    assert_eq!(
        session.total().amount() + reservation.total().amount(),
        dec("8.75")
    );
    assert_eq!(
        session.gross().amount() + reservation.gross().amount(),
        dec("10.00")
    );
}

/// `examples/tariff_17_reservation_with_expire_fee.json`.
///
/// > If the driver did not start a charging session and the reservation expired
/// > after the reserved time of 1 hour, the tariff would have resulted in costs
/// > of € 6.00 (excl. VAT) or € 7.20 (incl. VAT).
///
/// | Dimension | Quantity | Price ex VAT | Cost ex VAT | VAT |
/// |---|---|---|---|---|
/// | Flat | 1 | 4.00 | 4.00 | 20 % |
/// | Time | 60 minutes | 2.00 per hour | 2.00 | 20 % |
///
/// The tariff's unrestricted € 0.50 session fee is **not** on that list. There
/// is no charging session, so it has no subject — and a stack that reached for
/// it would bill € 6.50.
#[test]
fn an_expired_reservation_takes_the_expiry_fee_and_nothing_from_the_session_leg() {
    let t = tariff(vec![
        reserved(
            ReservationRestriction::ReservationExpires,
            vec![PriceComponent::new(Dimension::Flat, dec("4.00")).with_vat(dec("20"))],
        ),
        reserved(
            ReservationRestriction::Reservation,
            vec![
                PriceComponent::new(Dimension::Time, dec("2.00"))
                    .with_vat(dec("20"))
                    .with_step_size(600),
            ],
        ),
        session_leg(),
    ]);

    // Honoured, twenty-two minutes: the block is ten minutes, so thirty.
    let held = Reservation::honoured(
        datetime!(2026-01-02 10:00 +1),
        datetime!(2026-01-02 10:22 +1),
    );
    let used = rate_reservation(&t, &held);
    assert_eq!(used.total().to_string(), "1.00 EUR");
    assert_eq!(
        line(&used, Dimension::Flat, "4.00"),
        None,
        "the expiry fee is not owed by a reservation that was used"
    );
    let session = rate(&t, &twenty_kwh());
    assert_eq!(
        session.total().amount() + used.total().amount(),
        dec("6.50")
    );
    assert_eq!(
        session.gross().amount() + used.gross().amount(),
        dec("7.30")
    );

    // Expired after the full reserved hour.
    let lapsed = Reservation::expired(
        datetime!(2026-01-02 10:00 +1),
        datetime!(2026-01-02 11:00 +1),
    );
    let expired = rate_reservation(&t, &lapsed);
    assert_eq!(
        line(&expired, Dimension::Flat, "4.00").map(|(q, _)| q),
        Some(dec("1")),
        "the expiry fee, from the RESERVATION_EXPIRES element"
    );
    assert_eq!(
        line(&expired, Dimension::Time, "2.00").map(|(q, _)| q),
        Some(dec("3600")),
        "and the hour, from the RESERVATION element, which states the dimension \
         the expiry element does not"
    );
    assert_eq!(
        line(&expired, Dimension::Flat, "0.50"),
        None,
        "there is no charging session, so the session fee has no subject"
    );
    assert_eq!(expired.total().to_string(), "6.00 EUR");
    assert_eq!(expired.gross().to_string(), "7.20 EUR");
}

/// `examples/tariff_18_reservation_with_expire_time.json`.
///
/// > If the driver did not start a charging session and the reservation expired
/// > after the reserved time of 1.5 hours, the tariff would have resulted in
/// > costs of € 9.00 (excl. VAT) or € 10.80 (incl. VAT).
///
/// Both elements price `TIME`, and the specification says which wins: *"the
/// time based cost of an expired reservation will be calculated based on the
/// `RESERVATION_EXPIRES` Tariff Element"* — € 6.00 an hour, not € 3.00.
#[test]
fn an_expiry_element_takes_the_dimension_it_states_from_the_reservation_element() {
    let t = tariff(vec![
        reserved(
            ReservationRestriction::ReservationExpires,
            vec![
                PriceComponent::new(Dimension::Time, dec("6.00"))
                    .with_vat(dec("20"))
                    .with_step_size(600),
            ],
        ),
        reserved(
            ReservationRestriction::Reservation,
            vec![
                PriceComponent::new(Dimension::Time, dec("3.00"))
                    .with_vat(dec("20"))
                    .with_step_size(600),
            ],
        ),
        session_leg(),
    ]);

    // Honoured, twenty-two minutes at the ordinary reservation rate.
    let used = rate_reservation(
        &t,
        &Reservation::honoured(
            datetime!(2026-01-02 10:00 +1),
            datetime!(2026-01-02 10:22 +1),
        ),
    );
    assert_eq!(used.total().to_string(), "1.50 EUR");
    let session = rate(&t, &twenty_kwh());
    assert_eq!(
        session.total().amount() + used.total().amount(),
        dec("7.00")
    );
    assert_eq!(
        session.gross().amount() + used.gross().amount(),
        dec("7.90")
    );

    // Expired after ninety minutes, at the expiry rate.
    let expired = rate_reservation(
        &t,
        &Reservation::expired(
            datetime!(2026-01-02 10:00 +1),
            datetime!(2026-01-02 11:30 +1),
        ),
    );
    assert_eq!(
        line(&expired, Dimension::Time, "6.00").map(|(q, _)| q),
        Some(dec("5400"))
    );
    assert_eq!(line(&expired, Dimension::Time, "3.00"), None);
    assert_eq!(expired.total().to_string(), "9.00 EUR");
    assert_eq!(expired.gross().to_string(), "10.80 EUR");
}
