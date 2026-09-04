//! Properties of [`split::into_periods`] that no single example can state, over
//! pseudo-random meter series, cuts and idle intervals.
//!
//! 1. **Conservation.** The slots sum to the session total, exactly. Not nearly:
//!    `[A6 §IV.1]` allocates each quarter hour to a different supplier's balance
//!    group, and a residual shoved into the last slot misattributes energy to
//!    whoever held it.
//! 2. **The slots partition the session.** They begin at the first reading, end
//!    at the last, and leave no gap and no overlap — so every second of the
//!    session is settled exactly once.
//! 3. **Cutting does not change the session.** The same series split with cuts
//!    and idle intervals, and without them, has the same total and the same
//!    market series; the cuts only divide slices further.
//! 4. **The market series is the grid.** One entry per Messperiode, labelled by
//!    its end `[PTB-A 50.7 §3.1.7.2]`, summing to the total.
//! 5. **A held register is held.** No slot inside an idle interval carries
//!    energy: the session says nothing flowed there, and a straight line drawn
//!    across it is the contradiction the CDR builder would refuse (D190).

use emob_core::{Direction, Energy, QuarterHour};
use emob_session::{MeterReading, MeterSeries, ReadingContext, split};
use rust_decimal::Decimal;
use time::macros::datetime;

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

    fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low + 1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.between(1, 100) <= percent
    }
}

const START: time::OffsetDateTime = datetime!(2026-01-02 10:07:23 +1);

fn seconds(n: u64) -> time::Duration {
    time::Duration::seconds(i64::try_from(n).expect("in range"))
}

/// One generated session: the readings, plus the state changes and idle
/// stretches a caller would pass beside them.
struct Generated {
    series: MeterSeries,
    cuts: Vec<time::OffsetDateTime>,
    idle: Vec<(time::OffsetDateTime, time::OffsetDateTime)>,
    total: Energy,
}

/// A session of between two and forty readings, some clock-aligned, some
/// periodic, with idle stretches in which the register does not move.
fn generate(rng: &mut Rng) -> Generated {
    // A register that has already run for a while, at the resolution a German
    // meter states: milli-kilowatt-hours.
    let mut register = Decimal::new(i64::try_from(rng.between(1, 9_000_000)).unwrap_or(1), 3);
    let mut at = START;

    let mut readings = vec![MeterReading::new(
        at,
        Energy::from_kwh(register).expect("non-negative"),
        Direction::Import,
        ReadingContext::TransactionBegin,
    )];
    let mut idle = Vec::new();
    let mut cuts = Vec::new();

    for _ in 0..rng.between(1, 40) {
        let step = seconds(rng.between(30, 1500));
        let quiet = rng.chance(25);
        if quiet {
            idle.push((at, at + step));
            cuts.push(at);
            cuts.push(at + step);
        } else {
            // Up to 350 kW, at whole watt-hours.
            let wh = rng.between(0, step.whole_seconds().unsigned_abs() * 97);
            register += Decimal::new(i64::try_from(wh).unwrap_or(0), 3);
        }
        at += step;
        readings.push(MeterReading::new(
            at,
            Energy::from_kwh(register).expect("non-negative"),
            Direction::Import,
            if rng.chance(40) {
                ReadingContext::SampleClock
            } else {
                ReadingContext::SamplePeriodic
            },
        ));
    }

    let last = readings.len() - 1;
    readings[last].context = ReadingContext::TransactionEnd;
    let series = MeterSeries::new(Direction::Import, readings).expect("ascending, non-decreasing");
    let total = series.total().expect("non-decreasing");
    Generated {
        series,
        cuts,
        idle,
        total,
    }
}

#[test]
fn the_slots_conserve_and_partition_the_session() {
    let mut rng = Rng(0x5911_7000_0000_0001);
    for case in 0..500 {
        let g = generate(&mut rng);
        let s = split::into_periods(&g.series, &g.cuts, &g.idle).expect("a splittable session");

        assert!(
            s.conserves(),
            "case {case}: the slots do not sum to the total"
        );
        assert_eq!(s.total, g.total, "case {case}");

        let first = s.slots.first().expect("at least one slot");
        let last = s.slots.last().expect("at least one slot");
        assert_eq!(first.from, START, "case {case}: the split starts late");
        assert_eq!(
            last.to,
            g.series.last().at,
            "case {case}: the split stops early"
        );
        for pair in s.slots.windows(2) {
            assert_eq!(
                pair[0].to, pair[1].from,
                "case {case}: a second is settled twice or not at all"
            );
        }
        for slot in &s.slots {
            assert!(slot.to > slot.from, "case {case}: a slot of no duration");
            assert_eq!(
                slot.quarter_hour,
                QuarterHour::containing(slot.from),
                "case {case}: a slot filed under the wrong Messperiode"
            );
        }
    }
}

#[test]
fn cutting_divides_the_session_and_does_not_change_it() {
    let mut rng = Rng(0x5911_7000_0000_0002);
    for case in 0..500 {
        let g = generate(&mut rng);
        let plain = split::into_quarter_hours(&g.series).expect("a splittable session");
        let cut = split::into_periods(&g.series, &g.cuts, &g.idle).expect("a splittable session");

        assert_eq!(plain.total, cut.total, "case {case}");
        assert!(plain.conserves() && cut.conserves(), "case {case}");
        assert!(
            cut.slots.len() >= plain.slots.len(),
            "case {case}: a cut removed a boundary"
        );
        // The market side settles a whole Messperiode against one balance
        // group, so however finely the slots were cut for pricing, the series
        // handed over is the same one.
        assert_eq!(
            plain.market_series().len(),
            cut.market_series().len(),
            "case {case}: cutting changed the number of Messperioden"
        );
        assert_eq!(
            cut.market_series().iter().map(|(_, e)| *e).sum::<Energy>(),
            g.total,
            "case {case}: the market series lost energy"
        );
        // Every Messperiode is labelled by its end, and they ascend.
        for pair in cut.market_series().windows(2) {
            assert!(pair[0].0 < pair[1].0, "case {case}");
        }
    }
}

#[test]
fn nothing_flows_while_the_session_says_nothing_flows() {
    let mut rng = Rng(0x5911_7000_0000_0003);
    for case in 0..500 {
        let g = generate(&mut rng);
        let s = split::into_periods(&g.series, &g.cuts, &g.idle).expect("a splittable session");

        for slot in &s.slots {
            let inside_idle = g
                .idle
                .iter()
                .any(|&(from, to)| slot.from >= from && slot.to <= to);
            if inside_idle {
                assert!(
                    slot.energy.is_zero(),
                    "case {case}: {} kWh attributed to an interval the session calls idle",
                    slot.energy
                );
            }
        }
    }
}
