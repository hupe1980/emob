//! The `MiSpeL` month, held to the figures the Bundesnetzagentur printed and to
//! the properties no single example can state.
//!
//! Every expected figure in the worked examples is one the Festlegung wrote
//! down: Anlage 1's Beispielrechnungen 1 and 2 (§ 2.1.5, p. 15 and p. 16) and
//! Anlage 2's two Rumpfjahre (§ 9.3, p. 56) — BNetzA 618-25-02 of 01.10.2026,
//! in the sibling `hems` corpus. The Rumpfjahr limits are printed "zur
//! vereinfachten Darstellung gerundet", so those are compared after rounding
//! to whole kilowatt-hours; everything else is exact.

use std::collections::BTreeSet;

use emob_core::{Direction, Energy, EvseId, Formelsatz, QuarterHour};
use emob_session::mispel::{
    self, Choice, Conditions, GridQuarter, Month, Refusal, Site, pauschal_limits, pauschal_year,
};
use emob_session::{Provenance, SessionSplit, Slot};
use rust_decimal::{Decimal, RoundingStrategy};
use time::macros::{date, datetime};

fn kwh(v: i64) -> Energy {
    Energy::from_kwh(Decimal::from(v)).expect("non-negative")
}

fn d(v: i64) -> Decimal {
    Decimal::from(v)
}

fn point(n: u32) -> EvseId {
    format!("DE*AB7*E840*{n:04}").parse().expect("an EVSE id")
}

/// November 2027 in Berlin: after the consent deadline and clear of any clock
/// change, 2 880 quarter hours.
const NOV: time::OffsetDateTime = datetime!(2027-11-01 00:00 +01:00);

fn quarter(i: i64) -> QuarterHour {
    QuarterHour::containing(NOV + time::Duration::minutes(15 * i))
}

/// A whole, quiet November of the grid meter, with `set` applied.
fn november(set: &[(i64, i64, i64)]) -> Vec<GridQuarter> {
    (0..2880)
        .map(|i| {
            let (draw, feed_in) = set
                .iter()
                .find(|(at, ..)| *at == i)
                .map_or((0, 0), |&(_, nb, ne)| (nb, ne));
            GridQuarter {
                quarter: quarter(i),
                draw: kwh(draw),
                feed_in: kwh(feed_in),
                spot_negative: false,
            }
        })
        .collect()
}

/// One session, one direction, measured slots at the given quarter hours.
fn session(direction: Direction, slots: &[(i64, i64)], provenance: Provenance) -> SessionSplit {
    let slots: Vec<Slot> = slots
        .iter()
        .map(|&(i, energy)| Slot {
            quarter_hour: quarter(i),
            from: quarter(i).start(),
            to: quarter(i).end(),
            energy: kwh(energy),
            direction,
            provenance,
        })
        .collect();
    SessionSplit {
        total: slots.iter().map(|s| s.energy).sum(),
        slots,
        direction,
    }
}

fn site(formelsatz: Formelsatz) -> Site {
    Site {
        formelsatz,
        covered: BTreeSet::from([point(1)]),
    }
}

/// An installation switched on 1 November 2027, the earliest any plant could
/// without consent.
fn conditions() -> Conditions {
    Conditions {
        assigned_from: date!(2027 - 11 - 01),
        first_assignment: false,
        operator_consent: false,
        state_aid_approved_on: None,
    }
}

fn net(
    formelsatz: Formelsatz,
    grid: &[GridQuarter],
    points: &[(EvseId, SessionSplit)],
) -> Result<Month, Refusal> {
    mispel::month(
        2027,
        time::Month::November,
        &site(formelsatz),
        &conditions(),
        grid,
        points,
    )
}

fn whole(v: Decimal) -> Decimal {
    v.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
}

// ── Anlage 1, § 2.1.5 ───────────────────────────────────────────────────────

/// Evidence for SR-107.
#[test]
fn sr_107_beispielrechnung_1_the_point_is_charged_from_the_grid_first() {
    // p. 15: "Im Stromspeicher werden 100 kWh Strom bei der Einspeicherung
    // verbraucht. Über die Entnahmestelle werden 130 kWh Strom aus dem Netz
    // bezogen. Außerdem werden in der Solaranlage 50 kWh erzeugt." The 50 kWh
    // from the roof are not metered under A2 — they are what Z1 does not see.
    let grid = november(&[(0, 130, 0)]);
    let month = net(
        Formelsatz::A2,
        &grid,
        &[(
            point(1),
            session(Direction::Import, &[(0, 100)], Provenance::Measured),
        )],
    )
    .expect("a month");
    let a2 = month.a2.expect("A2 figures");
    // "…werden die Speicherverbräuche von 100 kWh in dieser Viertelstunde
    // vollständig aus dem Netz bezogen (und nicht aus der Solaranlage)."
    assert_eq!(a2.grid_charged, d(100), "(9) = Σ(1)¼");
    assert_eq!(a2.plant_charged, d(0), "(10): nothing from the roof");
    // "Die übrigen 30 kWh Netzbezug … werden dadurch dem sonstigen Verbrauch
    // zugeordnet."
    assert_eq!(month.grid_draw - a2.grid_charged, d(30));
    assert_eq!(month.grid_draw, d(130), "(3)");
}

/// Evidence for SR-107.
#[test]
fn sr_107_beispielrechnung_2_the_feed_in_comes_out_of_the_point_first() {
    // p. 16: "Im Stromspeicher werden 100 kWh und in der Solaranlage 50 kWh
    // erzeugt. 80 kWh Strom werden an der Einspeisestelle ins Netz
    // eingespeist."
    let grid = november(&[(0, 0, 80)]);
    let month = net(
        Formelsatz::A2,
        &grid,
        &[(
            point(1),
            session(Direction::Export, &[(0, 100)], Provenance::Measured),
        )],
    )
    .expect("a month");
    let a2 = month.a2.expect("A2 figures");
    // "…stammt die Netzeinspeisung von 80 kWh in dieser Viertelstunde
    // vollständig aus der Speichererzeugung (und nicht aus der Solaranlage)."
    assert_eq!(a2.device_feed_in_base, d(80), "(11) = Σ(2)¼");
    assert_eq!(
        month.grid_feed_in - a2.device_feed_in_base,
        d(0),
        "none from the roof"
    );
    // "Die übrigen 20 kWh Speichererzeugung … werden dadurch dem sonstigen
    // Verbrauch zugeordnet."
    assert_eq!(a2.device_generation - a2.device_feed_in_base, d(20));
}

/// Evidence for SR-107.
#[test]
fn sr_107_the_two_rumpfjahre_of_section_9_3() {
    // p. 56: an 8 kWp roof, a change on 16 May of an ordinary year; the day of
    // the change counts to the Rumpfjahr before it. Days and (P1)R do not
    // depend on the Basisfall; (P3)R and (P4)R printed there are P1's.
    let before = pauschal_limits(d(8), date!(2027 - 01 - 01), date!(2027 - 05 - 16)).expect("P2");
    assert_eq!((before.days, before.summer_days), (136, 46));
    assert_eq!(whole(before.support_cap), d(1005), "(P1)R = 1.005 kWh");

    let after = pauschal_limits(d(8), date!(2027 - 05 - 17), date!(2027 - 12 - 31)).expect("P2");
    assert_eq!((after.days, after.summer_days), (229, 137));
    assert_eq!(whole(after.support_cap), d(2995), "(P1)R = 2.995 kWh");

    // A whole year is its own Rumpfjahr, exactly: (P1) = 8 · 500, and P2's
    // factor of 0,2 puts the band at 800 and the threshold at 4 800.
    let year = pauschal_limits(d(8), date!(2027 - 01 - 01), date!(2027 - 12 - 31)).expect("P2");
    assert_eq!(year.support_cap, d(4000));
    assert_eq!(year.factor, Decimal::new(2, 1));
    assert_eq!(year.indifference_band, d(800));
    assert_eq!(year.settlement_threshold, d(4800));
}

// ── The governed window ─────────────────────────────────────────────────────

/// Evidence for SR-110.
#[test]
fn sr_110_nothing_before_the_bekanntgabe_and_nothing_before_october_2027_without_consent() {
    let c = conditions();
    let refused = mispel::governed(
        Choice::Abgrenzung,
        &c,
        date!(2026 - 09 - 01),
        date!(2026 - 09 - 30),
    )
    .expect_err("before the rules");
    assert_eq!(refused, Refusal::BeforeTheRules);
    assert!(refused.to_string().contains("Tenorziffer 8"));

    let c = Conditions {
        assigned_from: date!(2027 - 03 - 01),
        ..conditions()
    };
    let refused = mispel::governed(
        Choice::Abgrenzung,
        &c,
        date!(2027 - 03 - 01),
        date!(2027 - 03 - 31),
    )
    .expect_err("no consent");
    assert!(refused.to_string().contains("Tenorziffer 9 a"), "{refused}");

    // With consent, the same month is whole.
    let c = Conditions {
        operator_consent: true,
        ..c
    };
    let g = mispel::governed(
        Choice::Abgrenzung,
        &c,
        date!(2027 - 03 - 01),
        date!(2027 - 03 - 31),
    )
    .expect("governed");
    assert!(!g.rumpf);
}

/// Evidence for SR-109.
#[test]
fn sr_109_the_pauschaloption_waits_for_the_month_after_state_aid() {
    let c = Conditions {
        assigned_from: date!(2027 - 01 - 01),
        operator_consent: true,
        ..conditions()
    };
    let refused = pauschal_year(2027, d(8), &c, &[]).expect_err("no approval");
    assert_eq!(refused, Refusal::NoStateAid);
    assert!(refused.to_string().contains("Tenorziffer 9 b"));

    let c = Conditions {
        state_aid_approved_on: Some(date!(2027 - 03 - 10)),
        ..c
    };
    let g = mispel::governed(
        Choice::Pauschal,
        &c,
        date!(2027 - 01 - 01),
        date!(2027 - 12 - 31),
    )
    .expect("governed");
    assert_eq!((g.first, g.rumpf), (date!(2027 - 04 - 01), true));

    let c = Conditions {
        state_aid_approved_on: Some(date!(2027 - 12 - 15)),
        ..c
    };
    assert_eq!(
        mispel::governed(
            Choice::Pauschal,
            &c,
            date!(2027 - 01 - 01),
            date!(2027 - 12 - 31)
        ),
        Err(Refusal::BeforeStateAid {
            applicable_from: date!(2028 - 01 - 01)
        })
    );
}

/// Evidence for SR-109: the year's arithmetic, once it is available.
#[test]
fn sr_109_a_pauschal_year_settles_only_feed_in_above_the_threshold_at_non_negative_prices() {
    let c = Conditions {
        assigned_from: date!(2027 - 01 - 01),
        operator_consent: true,
        state_aid_approved_on: Some(date!(2026 - 11 - 05)),
        first_assignment: false,
    };
    let start = datetime!(2027-01-01 00:00 +01:00);
    let quarters = 365 * 96;
    let grid: Vec<GridQuarter> = (0..quarters)
        .map(|i| {
            let (draw, feed_in, spot_negative) = match i {
                0..50 => (0, 100, false), // 5 000 kWh at non-negative prices
                50 => (0, 1000, true),    // 1 000 kWh at a negative price
                51 => (150, 0, false),
                _ => (0, 0, false),
            };
            GridQuarter {
                quarter: QuarterHour::containing(start + time::Duration::minutes(15 * i)),
                draw: kwh(draw),
                feed_in: kwh(feed_in),
                spot_negative,
            }
        })
        .collect();
    let year = pauschal_year(2027, d(8), &c, &grid).expect("a year");
    assert!(!year.governed.rumpf);
    assert_eq!(year.feed_in_at_non_negative_prices, d(5000), "(P7)");
    assert_eq!(year.above_threshold, d(200), "(P8) = 5 000 − 4 800");
    assert_eq!(
        year.settleable_feed_in,
        d(150),
        "(P10): no more than the draw"
    );
    assert_eq!(year.levied_grid_draw, d(0), "(P11)");

    assert_eq!(
        pauschal_limits(d(0), start.date(), start.date()),
        Err(Refusal::NoSolar)
    );
    assert_eq!(
        pauschal_limits(d(31), start.date(), start.date()),
        Err(Refusal::SolarTooLarge(d(31)))
    );
}

/// Evidence for SR-111.
#[test]
fn sr_111_a_switch_is_on_the_first_and_a_first_assignment_makes_a_rumpfmonat() {
    let grid = november(&[(0, 40, 0), (14 * 96, 7, 0)]);
    let switch = Conditions {
        assigned_from: date!(2027 - 11 - 15),
        ..conditions()
    };
    let refused = mispel::month(
        2027,
        time::Month::November,
        &site(Formelsatz::A11),
        &switch,
        &grid,
        &[],
    )
    .expect_err("mid-month switch");
    assert!(refused.to_string().contains("§ 11"), "{refused}");

    let first = Conditions {
        first_assignment: true,
        ..switch
    };
    let month = mispel::month(
        2027,
        time::Month::November,
        &site(Formelsatz::A11),
        &first,
        &grid,
        &[],
    )
    .expect("a Rumpfmonat");
    assert!(month.governed.rumpf);
    assert_eq!(month.governed.first, date!(2027 - 11 - 15));
    assert_eq!(
        month.grid_draw,
        d(7),
        "the 40 kWh of 1 November are not this Rumpfmonat's"
    );
}

// ── What the month refuses ──────────────────────────────────────────────────

/// Evidence for SR-112.
#[test]
fn sr_112_a_discharge_the_messkonzept_does_not_cover_is_refused_by_name() {
    let grid = november(&[(3, 0, 5)]);
    let stranger = point(9);
    for formelsatz in [Formelsatz::A2, Formelsatz::A11] {
        let refused = net(
            formelsatz,
            &grid,
            &[(
                stranger.clone(),
                session(Direction::Export, &[(3, 5)], Provenance::Measured),
            )],
        )
        .expect_err("uncovered");
        assert_eq!(
            refused,
            Refusal::UncoveredDischarge {
                point: stranger.clone(),
                quarter: quarter(3)
            }
        );
    }
    // A unidirectional point that only draws is sonstiger Verbrauch, not a refusal.
    let grid = november(&[(3, 5, 0)]);
    net(
        Formelsatz::A2,
        &grid,
        &[(
            stranger,
            session(Direction::Import, &[(3, 5)], Provenance::Measured),
        )],
    )
    .expect("an ordinary draw");
}

/// Evidence for SR-113.
#[test]
fn sr_113_an_interpolated_quarter_hour_is_not_a_measurement() {
    let grid = november(&[(3, 5, 0)]);
    let points = [(
        point(1),
        session(Direction::Import, &[(3, 5)], Provenance::Interpolated),
    )];
    assert_eq!(
        net(Formelsatz::A2, &grid, &points),
        Err(Refusal::NotMeasured {
            point: point(1),
            quarter: quarter(3)
        })
    );
    // A11 reads no Z2, so the points' slots decide nothing there.
    net(Formelsatz::A11, &grid, &points).expect("A11 has no Z2");
}

/// Evidence for SR-108.
#[test]
fn sr_108_the_grid_series_covers_every_quarter_hour_once() {
    let mut grid = november(&[]);
    grid.remove(1000);
    let refused = net(Formelsatz::A11, &grid, &[]).expect_err("a gap");
    assert!(matches!(
        refused,
        Refusal::GridIncomplete {
            expected: 2880,
            received: 2879,
            ..
        }
    ));

    let mut grid = november(&[]);
    grid.push(grid[7]);
    assert_eq!(
        net(Formelsatz::A11, &grid, &[]),
        Err(Refusal::GridDuplicate(quarter(7)))
    );

    // October 2027 ends with the 25-hour day: 2 980 quarter hours, not 2 976.
    let start = datetime!(2027-10-01 00:00 +02:00);
    let october: Vec<GridQuarter> = (0..2980)
        .map(|i| GridQuarter {
            quarter: QuarterHour::containing(start + time::Duration::minutes(15 * i)),
            draw: kwh(0),
            feed_in: kwh(0),
            spot_negative: false,
        })
        .collect();
    let c = Conditions {
        assigned_from: date!(2027 - 10 - 01),
        ..conditions()
    };
    mispel::month(
        2027,
        time::Month::October,
        &site(Formelsatz::A11),
        &c,
        &october,
        &[],
    )
    .expect("the long month is complete");
}

/// Evidence for SR-114.
#[test]
fn sr_114_energy_a_car_brought_from_elsewhere_is_taken_back_out() {
    let grid = november(&[(0, 10, 0), (1, 0, 30)]);
    let month = net(
        Formelsatz::A2,
        &grid,
        &[
            (
                point(1),
                session(Direction::Import, &[(0, 10)], Provenance::Measured),
            ),
            (
                point(1),
                session(Direction::Export, &[(1, 30)], Provenance::Measured),
            ),
        ],
    )
    .expect("a month");
    let a2 = month.a2.expect("A2");
    assert_eq!(a2.device_feed_in_base, d(30), "(11)");
    assert_eq!(a2.foreign_charge, d(20), "(12) = 30 − 10");
    assert_eq!(a2.device_feed_in_considered, d(10), "(13) = 30 − 20");
    assert_eq!(month.settleable_feed_in, d(10), "(16)");
    assert_eq!((month.levy_reducing, month.levied_grid_draw), (d(10), d(0)));

    // A11 cannot see Fremdtankstrom (§ 10.3.3) and nets the whole feed-in,
    // capped by the draw.
    let a11 = net(Formelsatz::A11, &grid, &[]).expect("A11");
    assert_eq!(a11.settleable_feed_in, d(30), "(16)A11 = (4)");
    assert_eq!(a11.levy_reducing, d(10), "(20)A11 = MIN[(16)A11 ; (3)]");
}

/// Evidence for SR-115.
#[test]
fn sr_115_the_nachweis_round_trips_whole() {
    let grid = november(&[(0, 10, 0), (1, 0, 30)]);
    let month = net(
        Formelsatz::A2,
        &grid,
        &[(
            point(1),
            session(Direction::Import, &[(0, 7)], Provenance::Measured),
        )],
    )
    .expect("a month");
    let json = serde_json::to_string(&month).expect("serialises");
    assert!(json.contains("\"2027-11-01\""), "{json}");
    let back: Month = serde_json::from_str(&json).expect("reads back");
    assert_eq!(back, month);
    assert_eq!(
        month.a2.expect("A2").efficiency,
        Decimal::new(85, 2),
        "(14)A2"
    );
}

// ── Over random months ──────────────────────────────────────────────────────

/// SplitMix64 — the workspace takes no `rand`, and a seeded sequence is what a
/// replayable property test wants anyway.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in Wh at meter resolution, as kWh with three decimals.
    fn energy(&mut self, max_wh: u64) -> Energy {
        let wh = i64::try_from(self.next() % (max_wh + 1)).unwrap_or(0);
        Energy::from_kwh(Decimal::new(wh, 3)).expect("non-negative")
    }
}

/// Evidence for SR-106 and SC-002.
#[test]
fn sr_106_the_netting_conserves_over_random_months() {
    let mut rng = Rng(0x0009_2027);
    for _ in 0..500 {
        let grid: Vec<GridQuarter> = (0..2880)
            .map(|i| GridQuarter {
                quarter: quarter(i),
                draw: if rng.next().is_multiple_of(3) {
                    rng.energy(40_000)
                } else {
                    Energy::ZERO
                },
                feed_in: if rng.next().is_multiple_of(4) {
                    rng.energy(40_000)
                } else {
                    Energy::ZERO
                },
                spot_negative: false,
            })
            .collect();
        let mut points = Vec::new();
        let (mut imported, mut exported) = (Decimal::ZERO, Decimal::ZERO);
        for _ in 0..(rng.next() % 12) {
            let direction = if rng.next().is_multiple_of(2) {
                Direction::Import
            } else {
                Direction::Export
            };
            let start = i64::try_from(rng.next() % 2800).unwrap_or(0);
            let slots: Vec<Slot> = (start..start + i64::try_from(rng.next() % 64).unwrap_or(0))
                .map(|i| Slot {
                    quarter_hour: quarter(i),
                    from: quarter(i).start(),
                    to: quarter(i).end(),
                    energy: rng.energy(11_000),
                    direction,
                    provenance: Provenance::Measured,
                })
                .collect();
            let total: Energy = slots.iter().map(|s| s.energy).sum();
            match direction {
                Direction::Import => imported += total.kwh(),
                Direction::Export => exported += total.kwh(),
            }
            points.push((
                point(1),
                SessionSplit {
                    slots,
                    total,
                    direction,
                },
            ));
        }

        for formelsatz in [Formelsatz::A2, Formelsatz::A11] {
            let m = net(formelsatz, &grid, &points).expect("a month");
            assert_eq!(
                m.levy_reducing + m.levied_grid_draw,
                m.grid_draw,
                "(20) + (21) = (3)"
            );
            assert!(m.levy_reducing <= m.grid_draw, "(20) ≤ (3)");
            assert!(m.levied_grid_draw >= Decimal::ZERO);
            if let Some(a2) = m.a2 {
                assert_eq!(
                    a2.device_consumption, imported,
                    "(5) is every covered import"
                );
                assert_eq!(
                    a2.device_generation, exported,
                    "(6) is every covered export"
                );
                assert_eq!(
                    a2.device_consumption - a2.device_generation,
                    imported - exported,
                    "import − export conserves"
                );
                assert!(
                    m.settleable_feed_in <= a2.device_feed_in_considered,
                    "(16) ≤ (13)"
                );
                assert!(
                    a2.device_feed_in_considered <= a2.device_feed_in_base,
                    "(13) ≤ (11)"
                );
                assert!(
                    a2.device_feed_in_base <= m.grid_feed_in.min(a2.device_generation),
                    "(11) ≤ min((4), (6))"
                );
                assert!(
                    a2.grid_charged <= m.grid_draw.min(a2.device_consumption),
                    "(9) ≤ min((3), (5))"
                );
            } else {
                assert_eq!(m.settleable_feed_in, m.grid_feed_in, "(16)A11 = (4)");
            }
        }
    }
}
