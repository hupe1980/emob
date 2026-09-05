//! Four properties of [`rate`] over the two tariff shapes
//! `the_price_is_a_function_of_the_session.rs` cannot generate: a
//! **`step_size`**, and components sitting in **different VAT categories**.
//!
//! Both are places where the arithmetic passes through a step function — a
//! ceiling, and a rounding to the currency's minor unit — and a step function
//! is where an arbitrarily small difference becomes an arbitrarily large one.
//! The older property file generates neither, so every one of its four thousand
//! cases ran on the smooth part of the curve.
//!
//! 1. **The price still does not depend on how finely the session was sliced.**
//!    The same statement that file makes, over the shapes it cannot build.
//!    Rating the same physical session as one period per stretch, as sevenths
//!    and as thirty-sevenths gives one total (D245, D246).
//! 2. **The tariff's own limits are answered together.** A total is never
//!    lifted above a maximum the tariff publishes, nor cut below a minimum it
//!    states — and where the two leave no room, the contradiction is a note
//!    rather than whichever bound was consulted first (D247).
//! 3. **A session never costs less than nothing**, whatever a maximum asks for.
//! 4. **The breakdown adds up**: gross is net plus tax, in every basis, and
//!    every line reproduces its own amount.
//!
//! # The tolerance, and why the bounds get one and the total does not
//!
//! The total is compared to a nanocent, because two slicings of one session
//! differ only by `Decimal`'s own last place and no price can see that.
//!
//! The bounds are compared to a **minor unit**, because they bind on the exact
//! total and are read back off [`Rated::net`] and [`Rated::gross`], which state
//! one figure per VAT category and round each — the divergence
//! [`Rated::total`] documents. Chasing that last cent would mean deciding the
//! adjustment from rounded category sums, which is D245 exactly: the bound
//! would become a function of the session's slicing again.

use emob_core::{Activity, Currency, Energy, TimeZone};
use emob_tariff::{
    Chargeable, Dimension, Period, PriceComponent, PriceLimit, Rated, RatingNote, Restrictions,
    Tariff, TariffElement, TariffKind, TaxIncluded, rate,
};
use rust_decimal::Decimal;
use time::macros::datetime;

/// How far apart two slicings of one session may state its exact total.
///
/// A **tolerance**, compared against a difference, for the reason the sibling
/// file states: rounding both figures first is a step function, and two totals
/// a few units in `Decimal`'s last place apart can straddle a half at the ninth
/// decimal and round to different figures — the failure this whole file is
/// about, in the assertion written to catch it (D286).
const TOLERANCE: Decimal = Decimal::from_parts(1, 0, 0, false, 9);

/// Whether two exact totals agree to within [`TOLERANCE`].
fn agree(left: Decimal, right: Decimal) -> bool {
    (left - right).abs() <= TOLERANCE
}

/// A seeded `SplitMix64`, as the sibling property file uses: the workspace
/// takes no `rand`, and a replayable property test wants a pure sequence.
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

    fn chance(&mut self, percent: u64) -> bool {
        self.between(1, 100) <= percent
    }
}

fn dec(units: u64, scale: u32) -> Decimal {
    Decimal::new(i64::try_from(units).unwrap_or(i64::MAX), scale)
}

/// A component priced between `low` and `high` cents, with a VAT rate drawn
/// from the four answers that matter — none stated, the German standard rate,
/// the reduced rate, and an explicit zero — and a block size a third of the
/// time.
///
/// The four rates are the point: one rate across a tariff makes the net and the
/// gross total proportional, and every question this file asks becomes the same
/// question asked twice.
fn component(rng: &mut Rng, dimension: Dimension, low: u64, high: u64) -> PriceComponent {
    let mut component = PriceComponent::new(dimension, dec(rng.between(low, high), 2));
    match rng.between(0, 3) {
        0 => {}
        1 => component = component.with_vat(Decimal::from(19)),
        2 => component = component.with_vat(Decimal::from(7)),
        _ => component = component.with_vat(Decimal::ZERO),
    }
    if rng.chance(30) {
        // Wh for energy, seconds for the time dimensions — the units
        // `[OCPI 2.3.0 §mod_cdrs_step_size]` states the field in.
        component =
            component.with_step_size(u32::try_from(rng.between(2, 1800)).expect("inside a u32"));
    }
    component
}

/// A bound stated as a net figure, as a gross figure, or as both — because the
/// two limbs bind separately and only the pair exercises that.
fn price_limit(rng: &mut Rng, low: u64, high: u64) -> PriceLimit {
    let gross = dec(rng.between(low, high), 2);
    match rng.between(0, 2) {
        0 => PriceLimit::gross(gross),
        1 => PriceLimit::net(gross),
        _ => PriceLimit::net_and_gross(gross / Decimal::from(2), gross),
    }
}

fn tariff(rng: &mut Rng) -> Tariff {
    let mut elements: Vec<TariffElement> = Vec::new();
    if rng.chance(60) {
        elements.push(TariffElement {
            components: vec![component(rng, Dimension::Energy, 10, 99)],
            restrictions: Restrictions {
                max_kwh: Some(dec(rng.between(1, 40), 0)),
                ..Restrictions::default()
            },
        });
    }
    if rng.chance(50) {
        elements.push(TariffElement {
            components: vec![component(rng, Dimension::ParkingTime, 100, 900)],
            restrictions: Restrictions {
                min_duration_s: Some(rng.between(1, 60) * 60),
                ..Restrictions::default()
            },
        });
    }
    // The specification's own advice: one unrestricted default per dimension,
    // last `[OCPI 2.3.0 §Tariff]`.
    elements.push(TariffElement::unrestricted(vec![
        component(rng, Dimension::Energy, 10, 99),
        component(rng, Dimension::Time, 100, 900),
        component(rng, Dimension::ParkingTime, 100, 900),
        component(rng, Dimension::Flat, 0, 200),
    ]));

    Tariff {
        id: "rounding".parse().expect("a valid tariff id"),
        currency: Currency::EUR,
        // Contract rather than ad-hoc: `check_afir` forbids some of these
        // shapes on a fast charger, and the arithmetic is the subject here.
        kind: TariffKind::Contract,
        time_zone: TimeZone::new("Europe/Berlin").expect("a bundled zone"),
        // All three bases, because the two limbs of a bound swap roles between
        // them and a party outside a tax regime reads neither rate.
        tax_included: match rng.between(0, 9) {
            0 => TaxIncluded::NotApplicable,
            n if n < 5 => TaxIncluded::Yes,
            _ => TaxIncluded::No,
        },
        elements,
        min_price: {
            let take = rng.chance(35);
            take.then(|| price_limit(rng, 0, 2_000))
        },
        max_price: {
            let take = rng.chance(35);
            take.then(|| price_limit(rng, 500, 9_000))
        },
        valid_from: None,
        valid_until: None,
    }
}

/// One physical session: a run of charging and idle stretches at second
/// resolution, described once and then sliced three ways.
struct Stretch {
    seconds: u64,
    energy: Decimal,
    charging: bool,
}

fn session(rng: &mut Rng) -> Vec<Stretch> {
    (0..rng.between(1, 5))
        .map(|_| {
            let charging = rng.chance(75);
            Stretch {
                seconds: rng.between(60, 5400),
                // Whole watt-hours, the resolution a meter states — which is
                // also what puts a `step_size` boundary exactly on a total,
                // where the ceiling is at its most brittle.
                energy: if charging {
                    dec(rng.between(0, 40_000), 3)
                } else {
                    Decimal::ZERO
                },
                charging,
            }
        })
        .collect()
}

/// The stretches as periods, each cut into `pieces` equal slices — the same
/// physical session at a different resolution.
///
/// The energy is divided by *cumulative difference*, so the pieces telescope
/// back to the stretch's own energy however many there are: the test must not
/// itself introduce the discrepancy it would then attribute to the engine.
fn sliced(stretches: &[Stretch], start: time::OffsetDateTime, pieces: u64) -> Chargeable {
    let mut periods = Vec::new();
    let mut at = start;
    for stretch in stretches {
        let pieces = pieces.clamp(1, stretch.seconds.max(1));
        let mut carried = Decimal::ZERO;
        for piece in 1..=pieces {
            let previous = stretch.seconds * (piece - 1) / pieces;
            let offset = stretch.seconds * piece / pieces;
            if offset <= previous {
                continue;
            }
            let cumulative =
                stretch.energy * Decimal::from(offset) / Decimal::from(stretch.seconds);
            periods.push(Period {
                start: at + time::Duration::seconds(i64::try_from(previous).expect("in range")),
                end: at + time::Duration::seconds(i64::try_from(offset).expect("in range")),
                energy: Energy::from_kwh(cumulative - carried).expect("non-negative"),
                activity: if stretch.charging {
                    Activity::Charging
                } else {
                    Activity::Parked
                },
            });
            carried = cumulative;
        }
        at += time::Duration::seconds(i64::try_from(stretch.seconds).expect("in range"));
    }
    Chargeable::new(periods).expect("the stretches are ordered and do not overlap")
}

/// The instant the generated sessions are anchored to — a Saturday evening in
/// the German winter, as the sibling property file uses.
const ANCHOR: time::OffsetDateTime = datetime!(2026-01-03 21:30 +1);

/// A start instant for one case, drawn across the whole week and the whole
/// clock.
///
/// The same correction as the sibling file's, for the same reason: a fixed
/// Saturday evening puts every generated session inside Saturday and Sunday, so
/// a weekend restriction never changes its answer mid-session and two thirds of
/// the clock is never covered at all (D285).
fn start_at(rng: &mut Rng) -> time::OffsetDateTime {
    let day = i64::try_from(rng.between(0, 6)).expect("in range");
    let minute = i64::try_from(rng.between(0, 24 * 60 - 1)).expect("in range");
    ANCHOR.replace_time(time::Time::MIDNIGHT)
        + time::Duration::days(day)
        + time::Duration::minutes(minute)
}

/// How many cases each property runs per seed.
const CASES: u32 = 2_000;

/// The seeds each property is run from.
///
/// More than one, and the reason is the shape of what is being looked for. Both
/// failures this file exists to keep out are **knife edges** — a quantity
/// landing exactly on a block boundary, a category total landing exactly on a
/// half cent — so a generator meets them at a rate that depends on its seed
/// rather than on its case count. Each of the four below was checked against
/// the code as it stood before the fix: between them they catch both, and
/// neither is caught by all four.
const SEEDS: [u64; 5] = [0xA5A5_1234, 1, 2, 3, 9];

/// Whether the tariff's own limits left no room on this session, in which case
/// neither bound is a promise the rating could keep.
fn contradicts(rated: &Rated) -> bool {
    rated.notes.iter().any(|note| {
        matches!(
            note,
            RatingNote::LimitsContradict { .. } | RatingNote::AdjustmentClampedAtZero { .. }
        )
    })
}

#[test]
fn a_block_size_and_a_second_vat_rate_do_not_make_the_price_a_function_of_the_slicing() {
    for seed in SEEDS {
        let mut rng = Rng(seed);
        for case in 0..CASES {
            let tariff = tariff(&mut rng);
            let stretches = session(&mut rng);
            let start = start_at(&mut rng);
            let coarse = rate(&tariff, &sliced(&stretches, start, 1)).exact_total();
            let middle = rate(&tariff, &sliced(&stretches, start, 7)).exact_total();
            let fine = rate(&tariff, &sliced(&stretches, start, 37)).exact_total();
            assert!(
                agree(coarse.amount(), middle.amount()),
                "seed {seed:#x} case {case}: one period per stretch and sevenths priced differently ({coarse} vs {middle})"
            );
            assert!(
                agree(coarse.amount(), fine.amount()),
                "seed {seed:#x} case {case}: one period per stretch and thirty-sevenths priced differently ({coarse} vs {fine})"
            );
        }
    }
}

#[test]
fn neither_bound_is_broken_by_the_one_that_was_answered_first() {
    let minor = dec(1, 2);
    for seed in SEEDS {
        let mut rng = Rng(seed);
        for case in 0..CASES {
            let tariff = tariff(&mut rng);
            let stretches = session(&mut rng);
            let start = start_at(&mut rng);
            for pieces in [1, 7, 37] {
                let rated = rate(&tariff, &sliced(&stretches, start, pieces));
                if contradicts(&rated) {
                    continue;
                }
                let (net, gross) = (rated.net().amount(), rated.gross().amount());
                if let Some(limit) = tariff.max_price {
                    if let Some(target) = limit.before_taxes {
                        assert!(
                            net <= target + minor,
                            "case {case}/{pieces}: net {net} is above the tariff's own maximum {target}"
                        );
                    }
                    if let Some(target) = limit.after_taxes {
                        assert!(
                            gross <= target + minor,
                            "case {case}/{pieces}: gross {gross} is above the tariff's own maximum {target}"
                        );
                    }
                }
                if tariff.min_price.is_some() && rated.lines.is_empty() {
                    continue;
                }
                if let Some(limit) = tariff.min_price {
                    if let Some(target) = limit.before_taxes {
                        assert!(
                            net + minor >= target,
                            "case {case}/{pieces}: net {net} is below the tariff's own minimum {target}"
                        );
                    }
                    if let Some(target) = limit.after_taxes {
                        assert!(
                            gross + minor >= target,
                            "case {case}/{pieces}: gross {gross} is below the tariff's own minimum {target}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_session_never_costs_less_than_nothing_and_the_breakdown_adds_up() {
    // The shapes this file exists for, counted. A property asserted over a
    // space that cannot break it is not a property: the generated-month suite
    // added exactly this statement and passed it over **zero** cases, because
    // its own ceiling floor put the shape out of reach (D285).
    let mut bounds_bound = 0usize;
    let mut bounds_spread = 0usize;
    let mut blocks_rounded = 0usize;
    let mut two_categories = 0usize;

    for seed in SEEDS {
        let mut rng = Rng(seed);
        for case in 0..CASES {
            let tariff = tariff(&mut rng);
            let stretches = session(&mut rng);
            let start = start_at(&mut rng);
            for pieces in [1, 7, 37] {
                let rated = rate(&tariff, &sliced(&stretches, start, pieces));
                assert!(
                    rated.total().amount() >= Decimal::ZERO,
                    "case {case}/{pieces}: a maximum turned the session into a payment to the driver"
                );
                assert!(
                    rated.lines_reconcile(),
                    "case {case}/{pieces}: a line does not reproduce its own amount"
                );
                assert_eq!(
                    rated.gross().amount(),
                    rated.net().amount() + rated.tax().amount(),
                    "case {case}/{pieces}: the VAT breakdown does not add up"
                );

                // **No VAT category is owed a negative amount.** A bound is
                // attributed to one category and can be deeper than that
                // category holds; expressed as one allowance the document then
                // states a negative BT-116, which reconciles, which all 317 of
                // EN 16931's own rules accept, and which no tax office does
                // (D283). Stated here rather than only on the invoice, because
                // the breakdown crosses a roaming wire too — a partner was
                // being sent the negative as well.
                for line in rated.tax_summary() {
                    assert!(
                        !line.net.is_sign_negative(),
                        "case {case}/{pieces}: the {} % category is owed {} — a negative taxable \
                         amount under a positive total",
                        line.rate,
                        line.net
                    );
                }

                if pieces == 1 {
                    if rated.adjustment.is_some() {
                        bounds_bound += 1;
                    }
                    if rated.adjustment_parts().len() > 1 {
                        bounds_spread += 1;
                    }
                    if [Dimension::Energy, Dimension::Time, Dimension::ParkingTime]
                        .into_iter()
                        .any(|d| !rated.block_surplus_for(d).is_zero())
                    {
                        blocks_rounded += 1;
                    }
                    let mut rates: Vec<Option<Decimal>> =
                        rated.lines.iter().map(|line| line.vat).collect();
                    rates.sort_unstable();
                    rates.dedup();
                    if rates.len() > 1 {
                        two_categories += 1;
                    }
                }
            }
        }
    }

    // A generator that cannot reach a shape is a generator whose properties say
    // nothing about it.
    let cases = SEEDS.len() * CASES as usize;
    assert!(
        bounds_bound * 20 > cases,
        "only {bounds_bound} of {cases} sessions had a bound that bound, so the limit properties \
         above ran on sessions no limit touched"
    );
    assert!(
        bounds_spread > 50,
        "only {bounds_spread} sessions had a bound deeper than the category it is attributed to, \
         which is the shape that produced an invoice all 317 rules accept and no tax office does"
    );
    assert!(
        blocks_rounded > 50,
        "only {blocks_rounded} sessions rounded a quantity up to a block, so the step function \
         this file is named after was barely exercised"
    );
    assert!(
        two_categories * 4 > cases,
        "only {two_categories} of {cases} sessions were priced in more than one VAT category, and \
         one rate makes the net and the gross proportional — every question here becomes the same \
         question asked twice"
    );
}
