//! **A published bound binds in the basis it was published in.**
//!
//! `[OCPI 2.3.0 §mod_tariffs_pricelimit_class]` states a limit before taxes and
//! one after taxes and says both apply — *"as a rule, they both apply"* — so a
//! tariff quoting net prices with a **gross** ceiling has to reach that ceiling
//! through the tax, and one quoting gross prices with a net ceiling the other
//! way. The limb in the tariff's own basis is a subtraction and cannot be wrong;
//! the other one is an inversion, and that is where a ceiling goes missing.
//!
//! # Why an example could not state this
//!
//! Reaching the other basis needs to know what a unit of movement does to it,
//! which is a property of the VAT category the movement lands in — so the
//! answer is piecewise-linear in how deep the cut goes, with a slope change at
//! every category boundary. A tariff with one VAT rate makes the two limbs
//! proportional and the whole question invisible; every hand-written example in
//! this crate had one rate, or two that were both ordinary.
//!
//! The shape that broke it was a rate of exactly **−100 %**, which a partner's
//! document may carry and this crate treats as data rather than as a promise. On
//! gross lines that rate has no reciprocal, and the conversion refused it — for
//! both bases, though on **net** lines it is an ordinary multiplication by zero
//! with no hole in it. Refusing there dropped the whole limb, and a tariff with a
//! published gross `max_price` was not capped at all (D295).

use emob_core::{Currency, Energy, TimeZone};
use emob_tariff::{
    AdjustmentKind, Chargeable, Dimension, Period, PriceComponent, PriceLimit, RatingNote, Tariff,
    TariffElement, TariffKind, TaxIncluded, rate,
};
use rust_decimal::Decimal;
use time::macros::datetime;

/// One minor unit of slack, because [`Rated::net`] and [`Rated::gross`] are the
/// figures a *document* states and each rounds its categories. The bound is
/// solved against the exact totals, which is the whole of D245.
const SLACK: Decimal = Decimal::from_parts(1, 0, 0, false, 2);

/// SplitMix64 — the workspace takes no `rand`, and a replayable property test
/// wants a seeded sequence anyway.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low + 1)
    }
}

fn dec(units: i64, scale: u32) -> Decimal {
    Decimal::new(units, scale)
}

/// The rates a partner's document actually carries, plus the one that has no
/// reciprocal.
///
/// A rate **below** −100 % is deliberately absent: it makes the other basis fall
/// as the cut deepens, and an inversion of a function that is not monotone has
/// no unique answer. That limb is documented as not binding, and the tariff's
/// own basis still does.
fn vat(rng: &mut Rng) -> Decimal {
    match rng.between(0, 5) {
        0 => Decimal::ZERO,
        1 => dec(700, 2),
        2 => dec(1900, 2),
        3 => dec(2100, 2),
        4 => dec(550, 2),
        _ => dec(-10000, 2),
    }
}

fn limit(rng: &mut Rng, ceiling: bool) -> Option<PriceLimit> {
    let high = if ceiling { 3000 } else { 600 };
    let shape = rng.between(0, 3);
    let first = dec(i64::try_from(rng.between(0, high)).unwrap_or(0), 2);
    let second = dec(i64::try_from(rng.between(0, high)).unwrap_or(0), 2);
    match shape {
        0 => None,
        1 => Some(PriceLimit::net(first)),
        2 => Some(PriceLimit::gross(first)),
        _ => Some(PriceLimit::net_and_gross(first, second)),
    }
}

#[test]
fn every_published_bound_binds_in_the_basis_it_was_published_in() {
    let mut rng = Rng(0x_B0_1D_u64);
    let zone = TimeZone::new("Europe/Berlin").expect("a zone");
    let mut broken = Vec::new();

    for case in 0..6000_u64 {
        let basis = match rng.between(0, 2) {
            0 => TaxIncluded::No,
            1 => TaxIncluded::Yes,
            _ => TaxIncluded::NotApplicable,
        };
        let tariff = Tariff {
            id: "bound".parse().expect("a tariff id"),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: zone.clone(),
            tax_included: basis,
            elements: vec![TariffElement::unrestricted(vec![
                PriceComponent::new(
                    Dimension::Energy,
                    dec(i64::try_from(rng.between(1, 120)).unwrap_or(1), 2),
                )
                .with_vat(vat(&mut rng)),
                PriceComponent::new(
                    Dimension::Flat,
                    dec(i64::try_from(rng.between(0, 2500)).unwrap_or(0), 2),
                )
                .with_vat(vat(&mut rng)),
                PriceComponent::new(
                    Dimension::Time,
                    dec(i64::try_from(rng.between(0, 900)).unwrap_or(0), 2),
                )
                .with_vat(vat(&mut rng)),
            ])],
            min_price: limit(&mut rng, false),
            max_price: limit(&mut rng, true),
            valid_from: None,
            valid_until: None,
        };

        let minutes = i64::try_from(rng.between(1, 300)).unwrap_or(1);
        let session = Chargeable::new(vec![Period::charging(
            datetime!(2026-06-01 10:00 +2),
            datetime!(2026-06-01 10:00 +2) + time::Duration::minutes(minutes),
            Energy::from_kwh(dec(i64::try_from(rng.between(0, 90_000)).unwrap_or(0), 3))
                .expect("a non-negative energy"),
        )])
        .expect("one period is a session");

        let rated = rate(&tariff, &session);

        // ── The escape hatch is itself an assertion ────────────────────────
        //
        // A limb the rating cannot solve for is a real answer, and it is the
        // one a weak property would hide behind: skip every note and reverting
        // the fix changes nothing, because the defect *was* a limb that did not
        // bind. So the note is bounded before it is honoured.
        //
        // No rate here is below −100 %, so no slope runs backwards, so a **cut**
        // always has an answer: the walk spends a flat segment and moves on. A
        // maximum that reports itself unreachable is the regression this file
        // exists for.
        let unreachable: Vec<_> = rated
            .notes
            .iter()
            .filter_map(|note| match note {
                RatingNote::BoundUnreachable { kind, .. } => Some(*kind),
                _ => None,
            })
            .collect();
        assert!(
            !unreachable.contains(&AdjustmentKind::Maximum),
            "case {case}: a published maximum was not applied on a tariff whose every rate has a \
             positive factor or a flat one — a cut walks a flat segment and moves on"
        );
        // …and a minimum only fails to lift where the category it lands in
        // cannot move the other basis at all, which needs the dead rate.
        let has_dead_rate = tariff.elements[0]
            .components
            .iter()
            .any(|component| component.vat == Some(dec(-10000, 2)));
        assert!(
            unreachable.is_empty() || has_dead_rate,
            "case {case}: a bound was reported unreachable on a tariff every rate of which splits"
        );
        // …and it says so twice: the rate is named as well as the bound.
        if !unreachable.is_empty() {
            assert!(
                rated
                    .notes
                    .iter()
                    .any(|note| matches!(note, RatingNote::VatRateNotUsable { .. })),
                "case {case}: a bound reported unreachable with no note naming the rate that \
                 made it so"
            );
        }

        // A tariff contradicting itself, a cap deeper than the session, and the
        // floor that cannot be lifted to are document faults the rating names.
        if rated.notes.iter().any(|note| {
            matches!(
                note,
                RatingNote::LimitsContradict { .. }
                    | RatingNote::AdjustmentClampedAtZero { .. }
                    | RatingNote::BoundUnreachable { .. }
            )
        }) {
            continue;
        }

        let net = rated.net().amount();
        let gross = rated.gross().amount();
        let mut fail = |what: &str| {
            broken.push(format!(
                "case {case}: {what} — {basis:?}, net {net}, gross {gross}, \
                 min {:?}, max {:?}",
                tariff.min_price.map(|l| l.to_string()),
                tariff.max_price.map(|l| l.to_string()),
            ));
        };

        if let Some(max) = tariff.max_price {
            if max.before_taxes.is_some_and(|bound| net > bound + SLACK) {
                fail("the net total is above a published net maximum");
            }
            if max.after_taxes.is_some_and(|bound| gross > bound + SLACK) {
                fail("the gross total is above a published gross maximum");
            }
        }
        if let Some(min) = tariff.min_price {
            if min.before_taxes.is_some_and(|bound| net + SLACK < bound) {
                fail("the net total is below a published net minimum");
            }
            if min.after_taxes.is_some_and(|bound| gross + SLACK < bound) {
                fail("the gross total is below a published gross minimum");
            }
        }
    }

    assert!(
        broken.is_empty(),
        "{} of 6000 sessions were priced outside a bound the tariff publishes:\n{}",
        broken.len(),
        broken
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
