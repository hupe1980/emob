//! `rate` does not panic, on any input that passed a door.
//!
//! `Decimal` panics on overflow rather than saturating, and every figure here is
//! multiplied — by a price, by a VAT factor, by a thousand to reach watt-hours.
//! Both operands arrive from a **partner**: the quantity on a charge detail
//! record, the price and the published limits on the tariff beside it. The
//! sibling property files generate both in the range a charge point produces,
//! which is the range where none of this matters.
//!
//! Two doors, and this file is what proves each is load-bearing (D305):
//! `Energy` is bounded where it is built, and a price — which is not a
//! measurement and has no natural bound — is checked where it is used, leaving
//! its dimension unbilled *with a sentence* rather than taking the process down.

use emob_core::{Activity, Currency, Energy, QuantityError, TimeZone};
use emob_tariff::{
    Chargeable, Dimension, Period, PriceComponent, PriceLimit, Rated, RatingNote, Restrictions,
    Tariff, TariffElement, TariffKind, TaxIncluded, rate,
};
use rust_decimal::Decimal;
use time::macros::datetime;

/// The largest energy a charge point could ever have delivered, against the
/// bound: a Megawatt Charging System unit at 3.75 MW, flat out, for thirty
/// years.
#[test]
fn the_bound_is_past_anything_a_charge_point_can_deliver() {
    let thirty_years_of_hours = Decimal::from(30 * 365 * 24);
    let largest_charger_kw = Decimal::from(3750);
    let lifetime = thirty_years_of_hours * largest_charger_kw;
    assert!(
        lifetime < Energy::MAX_KWH,
        "the bound must not be reachable by a real machine: {lifetime} kWh"
    );
    // …and not by orders of magnitude, so it is a line past which a number
    // stopped being a measurement rather than a limit somebody will hit.
    assert!(lifetime * Decimal::from(10_000) > Energy::MAX_KWH);

    Energy::from_kwh(lifetime).expect("a lifetime of full-power charging is an energy");
}

/// The door itself: a magnitude no meter states is refused where it is built,
/// and the refusal names the bound rather than saying "invalid".
#[test]
fn a_quantity_no_meter_states_is_refused_at_the_door() {
    let absurd = Decimal::MAX / Decimal::from(10);
    let refused = Energy::from_kwh(absurd).expect_err("a peer does not get to choose this");
    assert!(
        matches!(refused, QuantityError::Unbillable { what: "energy", .. }),
        "{refused:?}"
    );
    let sentence = refused.to_string();
    assert!(sentence.contains("1000000000000"), "{sentence}");
    assert!(sentence.contains("cannot be multiplied"), "{sentence}");

    // The same door, whichever unit it is entered through — a partner sending
    // watt-hours must not get past a check that only reads kWh.
    assert!(Energy::from_wh(absurd).is_err());

    // And through the wire, which is the door that was walked past once before
    // (D264): a derived `Deserialize` is a constructor nobody wrote.
    let json = format!("{absurd}");
    assert!(
        serde_json::from_str::<Energy>(&json).is_err(),
        "a record carrying {absurd} kWh must not deserialise"
    );

    // What is exactly on the bound is a measurement, not a refusal.
    assert!(Energy::from_kwh(Energy::MAX_KWH).is_ok());
}

fn session(kwh: Decimal) -> Chargeable {
    let start = datetime!(2026-01-03 21:30 +1);
    Chargeable::new(vec![Period {
        start,
        end: start + time::Duration::hours(1),
        energy: Energy::from_kwh(kwh).expect("inside the bound"),
        activity: Activity::Charging,
    }])
    .expect("one period is ordered and does not overlap")
}

fn tariff(price: Decimal, step_size: u32) -> Tariff {
    at_the_corners(
        price,
        step_size,
        Decimal::from(19),
        TaxIncluded::No,
        None,
        None,
    )
}

fn at_the_corners(
    price: Decimal,
    step_size: u32,
    vat: Decimal,
    basis: TaxIncluded,
    min_price: Option<PriceLimit>,
    max_price: Option<PriceLimit>,
) -> Tariff {
    Tariff {
        id: "overflow".parse().expect("a valid tariff id"),
        currency: Currency::EUR,
        kind: TariffKind::Contract,
        time_zone: TimeZone::new("Europe/Berlin").expect("a bundled zone"),
        tax_included: basis,
        elements: vec![TariffElement {
            components: vec![
                PriceComponent::new(Dimension::Energy, price)
                    .with_vat(vat)
                    .with_step_size(step_size),
                // A second dimension, because the totals are sums over lines
                // and the headroom argument is about how many there can be.
                PriceComponent::new(Dimension::Time, price).with_vat(vat),
            ],
            restrictions: Restrictions::default(),
        }],
        min_price,
        max_price,
        valid_from: None,
        valid_until: None,
    }
}

/// …and the bound arithmetic, which is the other place a figure is multiplied:
/// `[OCPI 2.3.0 §mod_tariffs_pricelimit_class]` states one ceiling before taxes
/// and one after, and answering both moves a limb through the VAT factor.
#[test]
fn a_price_limit_at_the_edge_is_answered_rather_than_fatal() {
    let extremes = [
        Decimal::ZERO,
        Decimal::MAX,
        Decimal::MAX / Decimal::from(2),
        Decimal::from(-1),
    ];
    for basis in [
        TaxIncluded::No,
        TaxIncluded::Yes,
        TaxIncluded::NotApplicable,
    ] {
        for vat in [
            Decimal::ZERO,
            Decimal::from(19),
            Decimal::from(-100),
            Decimal::MAX,
            Decimal::MAX / Decimal::from(-2),
        ] {
            for limit in extremes {
                for quantity in [Decimal::ONE, Energy::MAX_KWH] {
                    let tariff = at_the_corners(
                        Decimal::new(50, 2),
                        1,
                        vat,
                        basis,
                        Some(PriceLimit::net_and_gross(limit, limit)),
                        Some(PriceLimit::gross(limit)),
                    );
                    let rated = rate(&tariff, &session(quantity));
                    let _ = (rated.total(), rated.gross(), rated.net());
                }
            }
        }
    }
}

/// The whole claim, at the corners: the largest quantity the door admits,
/// against prices from nothing to past what a decimal can multiply it by.
#[test]
fn rate_answers_at_every_corner_of_what_the_doors_admit() {
    let quantities = [
        Decimal::ZERO,
        Decimal::ONE,
        Decimal::new(1, 3),
        Energy::MAX_KWH,
        Energy::MAX_KWH - Decimal::ONE,
    ];
    let prices = [
        Decimal::ZERO,
        Decimal::new(50, 2),
        Decimal::from(1_000_000),
        Decimal::MAX,
        Decimal::MAX / Decimal::from(2),
    ];
    // Including the block rounding, which multiplies the quantity by a
    // thousand to reach watt-hours before it divides.
    for step_size in [1_u32, 500, u32::MAX] {
        for quantity in quantities {
            for price in prices {
                let rated: Rated = rate(&tariff(price, step_size), &session(quantity));
                // An answer, whatever it is — and a total that is a number.
                let _ = rated.total();
                let _ = rated.gross();
                let _ = rated.net();
                // Every line still explains its own amount: a refusal drops a
                // line, it does not leave a line the arithmetic disagrees with.
                for line in &rated.lines {
                    assert!(line.reconciles(), "{line:?}");
                }
            }
        }
    }
}

/// …and where it refuses, the refusal is the loud kind. A dimension dropped
/// silently is an invoice short by the whole session with nothing saying so.
#[test]
fn a_price_nothing_can_multiply_leaves_its_dimension_unbilled_and_says_so() {
    let rated = rate(&tariff(Decimal::MAX, 1), &session(Decimal::from(30)));

    let note = rated
        .notes
        .iter()
        .find(|note| matches!(note, RatingNote::PriceOutOfRange { .. }))
        .expect("a price that cannot be applied is reported");
    let sentence = note.to_string();
    assert!(sentence.contains("no decimal here can hold"), "{sentence}");
    assert!(sentence.contains("check the tariff"), "{sentence}");

    // Nothing was charged for it, rather than something invented.
    assert!(rated.total().amount().is_zero(), "{:?}", rated.total());
    assert!(rated.amount_for(Dimension::Energy).is_none());
    assert!(rated.amount_for(Dimension::Time).is_none());

    // And an ordinary price on the same session still bills it, so the refusal
    // is about this price rather than about the session.
    let ordinary = rate(&tariff(Decimal::new(50, 2), 1), &session(Decimal::from(30)));
    // 30 kWh at 0.50, and one hour of it at 0.50 an hour.
    assert_eq!(ordinary.total().amount(), Decimal::new(1550, 2));
    assert!(
        !ordinary
            .notes
            .iter()
            .any(|note| matches!(note, RatingNote::PriceOutOfRange { .. }))
    );
}
