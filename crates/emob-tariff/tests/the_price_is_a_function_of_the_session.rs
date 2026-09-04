//! Three properties of [`rate`] that no single example can state, over
//! pseudo-random tariffs and sessions.
//!
//! 1. **The price does not depend on how finely the session was sliced.** A
//!    tariff whose restrictions all read quantities that *accumulate* — energy,
//!    elapsed time, the wall clock — has one answer for one physical session,
//!    and cutting each period at the tariff's own thresholds is what makes that
//!    true. Rate the same session as one period per stretch, as quarters and as
//!    thirty-sevenths, and the totals agree.
//! 2. **Every quantity is either priced or named.** The base quantity across
//!    the lines, plus what the notes report as unpriced, plus what they report
//!    as below the clock's resolution, is the session's own. A kilowatt-hour
//!    that is neither charged for nor named is one that vanished.
//! 3. **Every line reproduces its own amount**, which is the invariant a
//!    receiving party checks before it disputes a total, and the total is the
//!    lines plus at most the one adjustment.
//!
//! # The one restriction excluded, and the four digits given up
//!
//! Average power is not generated here, and the exclusion is the point: a
//! period carries no information about the power inside it, so a finer slicing
//! is a *better measurement* rather than a different answer to the same
//! question. [`RatingNote::PowerJudgedPerPeriod`] says so on any tariff that
//! carries one.
//!
//! And the quantities are compared to nine decimal places rather than to the
//! last. Where a threshold falls inside a period the energy either side of it is
//! apportioned by `emob_core::apportion`, which quotes a nanowatt-hour — so two
//! slicings placing their cuts in different seconds differ by a few of them, and
//! by nothing a price can see: the worst over four thousand cases here is
//! 8 × 10⁻¹³ of a euro. Asserting the *rounded* total instead would be a weaker
//! statement that flakes, because an exact total landing on a half cent rounds
//! either way on a residue that small.

use emob_core::{Activity, Currency, Energy, TimeZone};
use emob_tariff::{
    Chargeable, Dimension, Period, PriceComponent, PriceLimit, Rated, RatingNote, Restrictions,
    Tariff, TariffElement, TariffKind, TaxIncluded, rate,
};
use rust_decimal::Decimal;
use time::macros::datetime;

/// How far apart two slicings of one session may state its exact total.
const PLACES: u32 = 9;

/// A tiny deterministic generator — the workspace takes no `rand`, and a seeded
/// integer sequence is what a replayable property test wants anyway.
struct Rng(u64);

impl Rng {
    /// SplitMix64.
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

fn hour(rng: &mut Rng) -> time::Time {
    time::Time::from_hms(u8::try_from(rng.between(0, 23)).unwrap_or(0), 0, 0)
        .expect("a whole hour is a time")
}

/// A tariff with tiers on energy, on elapsed time, on the wall clock and on the
/// weekday — every restriction whose threshold a period *contains*, and none
/// whose threshold it does not.
/// A bound in the tariff's own (gross) basis, sometimes with the net limb too.
fn price_limit(rng: &mut Rng, gross: rust_decimal::Decimal) -> PriceLimit {
    if rng.chance(50) {
        // A plausible net figure below the gross one, so the pair does not
        // describe an impossible tariff.
        PriceLimit {
            before_taxes: Some(gross / rust_decimal::Decimal::from(2)),
            after_taxes: Some(gross),
        }
    } else {
        PriceLimit::gross(gross)
    }
}

fn tariff(rng: &mut Rng) -> Tariff {
    let mut elements: Vec<TariffElement> = Vec::new();

    if rng.chance(70) {
        elements.push(TariffElement {
            components: vec![PriceComponent::new(
                Dimension::Energy,
                dec(rng.between(10, 99), 2),
            )],
            restrictions: Restrictions {
                max_kwh: Some(dec(rng.between(1, 40), 0)),
                ..Restrictions::default()
            },
        });
    }
    if rng.chance(50) {
        elements.push(TariffElement {
            components: vec![PriceComponent::new(
                Dimension::ParkingTime,
                dec(rng.between(100, 900), 2),
            )],
            restrictions: Restrictions {
                min_duration_s: Some(rng.between(1, 60) * 60),
                ..Restrictions::default()
            },
        });
    }
    if rng.chance(50) {
        elements.push(TariffElement {
            components: vec![PriceComponent::new(
                Dimension::Energy,
                dec(rng.between(10, 99), 2),
            )],
            restrictions: Restrictions {
                start_time: Some(hour(rng)),
                end_time: Some(hour(rng)),
                ..Restrictions::default()
            },
        });
    }
    if rng.chance(40) {
        elements.push(TariffElement {
            components: vec![PriceComponent::new(
                Dimension::Time,
                dec(rng.between(100, 900), 2),
            )],
            restrictions: Restrictions {
                days_of_week: vec![time::Weekday::Saturday, time::Weekday::Sunday],
                ..Restrictions::default()
            },
        });
    }

    // A **reservation** element, which prices no session at all. It is here
    // because it is the shape most likely to be read as one: it carries `TIME`
    // and `FLAT` like an ordinary element and differs only by a restriction, so
    // an engine that forgets to ask the question prices a hold rate as charging
    // time. `matches_restrictions` refuses to cross the two populations, and
    // these properties are what says the refusal holds under everything else
    // the generator does — including in front of the unrestricted default,
    // where a per-element reading would stop (D250).
    if rng.chance(35) {
        elements.push(TariffElement {
            components: vec![
                PriceComponent::new(Dimension::Time, dec(rng.between(100, 900), 2)),
                PriceComponent::new(Dimension::Flat, dec(rng.between(0, 200), 2)),
            ],
            restrictions: Restrictions {
                reservation: Some(if rng.chance(50) {
                    emob_tariff::ReservationRestriction::Reservation
                } else {
                    emob_tariff::ReservationRestriction::ReservationExpires
                }),
                ..Restrictions::default()
            },
        });
    }

    // The specification's own advice: one unrestricted default per dimension,
    // last `[OCPI 2.3.0 §Tariff]`. Without it a session can go unpriced, which
    // is a lawful outcome and a duller test.
    elements.push(TariffElement::unrestricted(vec![
        PriceComponent::new(Dimension::Energy, dec(rng.between(10, 99), 2)),
        PriceComponent::new(Dimension::Time, dec(rng.between(100, 900), 2)),
        PriceComponent::new(Dimension::ParkingTime, dec(rng.between(100, 900), 2)),
        PriceComponent::new(Dimension::Flat, dec(rng.between(0, 200), 2)),
    ]));

    Tariff {
        id: "prop".parse().expect("a valid tariff id"),
        currency: Currency::EUR,
        // Contract rather than ad-hoc: `check_afir` forbids some of these
        // shapes on a fast charger, and the arithmetic is the subject here.
        kind: TariffKind::Contract,
        time_zone: TimeZone::new("Europe/Berlin").expect("a bundled zone"),
        tax_included: TaxIncluded::Yes,
        elements,
        // Gross prices, so the bound the tariff quotes is the after-tax one —
        // and a generated tariff states both limbs half the time, because the
        // two bind separately and only the pair exercises that.
        min_price: {
            let take = rng.chance(20);
            let amount = dec(rng.between(0, 500), 2);
            take.then(|| price_limit(rng, amount))
        },
        max_price: {
            let take = rng.chance(20);
            let amount = dec(rng.between(1000, 9000), 2);
            take.then(|| price_limit(rng, amount))
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
    (0..rng.between(1, 6))
        .map(|_| {
            let charging = rng.chance(75);
            Stretch {
                seconds: rng.between(60, 5400),
                // Whole watt-hours, the resolution a meter states.
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

/// The stretches as [`Chargeable`] periods, each stretch cut into `pieces`
/// equal slices — the same physical session at a different resolution.
///
/// The energy is divided by *cumulative difference*, so the pieces telescope
/// back to the stretch's own energy exactly however many there are: the test
/// must not itself introduce the discrepancy it would then attribute to the
/// engine. It is the same construction `emob_session::split` uses.
fn sliced(stretches: &[Stretch], start: time::OffsetDateTime, pieces: u64) -> Chargeable {
    let mut periods = Vec::new();
    let mut at = start;
    for stretch in stretches {
        // A stretch cannot be cut into more pieces than it has seconds.
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

/// What the session handed to [`rate`] actually did, in each dimension's base
/// unit.
///
/// Read off the [`Chargeable`] rather than off the stretches it was built from,
/// because that is the input the rating is answerable for.
///
/// Compared to [`PLACES`] rather than to the last digit, for the reason stated
/// at the top of this file: the two sums group the same pieces differently, and
/// `Decimal` carries ninety-six bits, so adding values that already spend
/// twenty-something places on a repeating fraction rounds the last of them.
fn measured(session: &Chargeable) -> [(Dimension, Decimal); 3] {
    [
        (Dimension::Energy, session.total_energy().kwh()),
        (Dimension::Time, Decimal::from(session.charging_seconds())),
        (
            Dimension::ParkingTime,
            Decimal::from(session.parking_seconds()),
        ),
    ]
}

/// The base quantity a rating accounted for in one dimension: what it priced,
/// plus what it named as unpriced, plus what it named as unresolvable.
fn accounted_for(rated: &Rated, dimension: Dimension) -> Decimal {
    let named: Decimal = rated
        .notes
        .iter()
        .filter_map(|note| match note {
            RatingNote::Unpriced {
                dimension: d,
                base_quantity,
                ..
            } if *d == dimension => Some(*base_quantity),
            // A span the station's clock cannot resolve is dropped as a line
            // with its reason `[REA 6-A §3.1]`, and the reason carries it.
            RatingNote::DurationBelowResolution {
                dimension: d,
                measured_seconds,
                ..
            } if *d == dimension => Some(*measured_seconds),
            _ => None,
        })
        .sum();
    rated.base_quantity_for(dimension) + named
}

/// A Saturday evening in the German winter, so the weekday and the wall-clock
/// restrictions both have something to bite on and long sessions cross local
/// midnight into a Sunday.
const START: time::OffsetDateTime = datetime!(2026-01-03 21:30 +1);

#[test]
fn the_total_does_not_depend_on_how_finely_the_session_was_sliced() {
    let mut rng = Rng(0x5EED_1234_ABCD_0001);

    for case in 0..2000 {
        let t = tariff(&mut rng);
        let stretches = session(&mut rng);

        let coarse = rate(&t, &sliced(&stretches, START, 1)).exact_total();
        let quarterly = rate(&t, &sliced(&stretches, START, 4)).exact_total();
        let fine = rate(&t, &sliced(&stretches, START, 37)).exact_total();

        for other in [quarterly, fine] {
            assert_eq!(
                coarse.amount().round_dp(PLACES),
                other.amount().round_dp(PLACES),
                "case {case}: one session, two slicings, two prices ({coarse} vs {other})"
            );
        }
    }
}

#[test]
fn a_tier_boundary_inside_a_period_too_short_to_hold_it_still_tiers() {
    // The regression the property found (D221). A 350 kW charge delivers
    // 0.1 kWh a second, so a two-second slice of it carries the fourth
    // kilowatt-hour of "the first 4 kWh at 0.30, the rest at 0.42" — and the
    // instant it crosses is not one two whole seconds can name. Requiring the
    // cut to advance the clock dropped it and charged the whole slice at 0.30.
    let t = Tariff {
        id: "tiered".parse().unwrap(),
        currency: Currency::EUR,
        kind: TariffKind::Contract,
        time_zone: TimeZone::new("Europe/Berlin").unwrap(),
        tax_included: TaxIncluded::Yes,
        elements: vec![
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec(30, 2))],
                restrictions: Restrictions {
                    max_kwh: Some(Decimal::from(4)),
                    ..Restrictions::default()
                },
            },
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec(42, 2))]),
        ],
        min_price: None,
        max_price: None,
        valid_from: None,
        valid_until: None,
    };

    let kwh = |s: &str| Energy::from_kwh(s.parse::<Decimal>().unwrap()).unwrap();
    let at = |s: u64| START + time::Duration::seconds(i64::try_from(s).unwrap());

    // Ten kilowatt-hours in a hundred seconds, as one period and as fifty
    // two-second slices of 0.2 kWh each. Four at 0.30 and six at 0.42 = 3.72.
    let one = Chargeable::new(vec![Period::charging(at(0), at(100), kwh("10.0"))]).unwrap();
    let fifty = Chargeable::new(
        (0..50)
            .map(|i| Period::charging(at(i * 2), at(i * 2 + 2), kwh("0.2")))
            .collect(),
    )
    .unwrap();

    assert_eq!(rate(&t, &one).exact_total().amount(), dec(372, 2));
    assert_eq!(
        rate(&t, &fifty).exact_total().amount(),
        dec(372, 2),
        "the fourth kilowatt-hour falls inside a two-second slice, and the tier still tiers"
    );
    // …and the boundary is exact: four kilowatt-hours at the first price.
    let fine = rate(&t, &fifty);
    assert_eq!(fine.lines.len(), 2, "{:?}", fine.lines);
    assert_eq!(fine.lines[0].base_quantity, Decimal::from(4));

    // The same at the resolution that has no interior second at all: a hundred
    // one-second periods. The cut is then degenerate in time — it opens a slice
    // of no duration — and still exact in kilowatt-hours, which is the trade.
    let hundred = Chargeable::new(
        (0..100)
            .map(|i| Period::charging(at(i), at(i + 1), kwh("0.1")))
            .collect(),
    )
    .unwrap();
    let second_by_second = rate(&t, &hundred);
    assert_eq!(second_by_second.exact_total().amount(), dec(372, 2));
    assert_eq!(
        second_by_second.base_quantity_for(Dimension::Energy),
        Decimal::from(10),
        "and the cut divides the session rather than changing it"
    );
}

#[test]
fn every_quantity_is_either_priced_or_named() {
    let mut rng = Rng(0x5EED_1234_ABCD_0002);

    for case in 0..2000 {
        let t = tariff(&mut rng);
        let chargeable = sliced(&session(&mut rng), START, 3);
        let rated = rate(&t, &chargeable);
        let priced = t.dimensions();

        for (dimension, actual) in measured(&chargeable) {
            // A dimension no element carries is not one this session can be
            // short of: `[OCPI 2.3.0 §Tariff]` answers it with "there will be
            // no costs for that Tariff Dimension", and ninety-six notes saying
            // so would be noise rather than a finding.
            if !priced.contains(&dimension) {
                continue;
            }
            assert_eq!(
                accounted_for(&rated, dimension).round_dp(PLACES),
                actual.round_dp(PLACES),
                "case {case}: {dimension} was neither charged for nor named"
            );
        }
    }
}

#[test]
fn every_line_reproduces_its_own_amount() {
    let mut rng = Rng(0x5EED_1234_ABCD_0003);

    for case in 0..2000 {
        let t = tariff(&mut rng);
        let rated = rate(&t, &sliced(&session(&mut rng), START, 5));
        assert!(
            rated.lines_reconcile(),
            "case {case}: a line does not explain its own amount: {:?}",
            rated.lines
        );
        // …and every term of the total is a line or the one adjustment.
        let lines: Decimal = rated.lines.iter().map(|l| l.amount).sum();
        assert_eq!(
            rated.exact_total().amount(),
            lines + rated.adjustment.map_or(Decimal::ZERO, |a| a.amount),
            "case {case}"
        );
    }
}
