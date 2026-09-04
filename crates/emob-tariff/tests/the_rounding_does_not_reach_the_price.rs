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
const PLACES: u32 = 9;

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

/// A Saturday evening in the German winter, as the sibling property file uses.
const START: time::OffsetDateTime = datetime!(2026-01-03 21:30 +1);

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
            let coarse = rate(&tariff, &sliced(&stretches, START, 1)).exact_total();
            let middle = rate(&tariff, &sliced(&stretches, START, 7)).exact_total();
            let fine = rate(&tariff, &sliced(&stretches, START, 37)).exact_total();
            assert_eq!(
                coarse.amount().round_dp(PLACES),
                middle.amount().round_dp(PLACES),
                "seed {seed:#x} case {case}: one period per stretch and sevenths priced differently"
            );
            assert_eq!(
                coarse.amount().round_dp(PLACES),
                fine.amount().round_dp(PLACES),
                "seed {seed:#x} case {case}: one period per stretch and thirty-sevenths priced differently"
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
            for pieces in [1, 7, 37] {
                let rated = rate(&tariff, &sliced(&stretches, START, pieces));
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
    for seed in SEEDS {
        let mut rng = Rng(seed);
        for case in 0..CASES {
            let tariff = tariff(&mut rng);
            let stretches = session(&mut rng);
            for pieces in [1, 7, 37] {
                let rated = rate(&tariff, &sliced(&stretches, START, pieces));
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
            }
        }
    }
}
