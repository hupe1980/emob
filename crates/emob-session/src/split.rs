//! Splitting a session across quarter hours, exactly.
//!
//! # Why this exists
//!
//! A charging session that runs from 10:07 to 11:23 has to be settled against
//! six quarter hours. Under German pass-through charging (NZR-EMob / Modell 2)
//! the operator assigns each quarter hour's energy to the balance group of the
//! supplier the driver chose, quarter hour by quarter hour `[A6 §IV.1]`, and
//! `mako-emob` will not accept a set of numbers that does not add up.
//!
//! So this module has one hard requirement and one honest one.
//!
//! # Conservation is by construction, not by reconciliation
//!
//! The naive approach computes each slot's energy independently and then
//! discovers the sum is a few milliwatt-hours off the session total, because
//! rounding happened six times. The usual fix is to shove the difference into
//! the last slot, which silently misattributes energy to whoever held 11:15.
//!
//! Instead, this computes the **cumulative** energy at each quarter-hour
//! *boundary* once, and takes differences:
//!
//! ```text
//! slot[i] = cumulative(boundary[i+1]) − cumulative(boundary[i])
//! ```
//!
//! The sum telescopes: every interior boundary appears once positive and once
//! negative and cancels exactly, whatever it was rounded to. What is left is
//! `cumulative(end) − cumulative(start)`, which is the session total, to the
//! last digit, always. [`SessionSplit::conserves`] proves it, and a property
//! test runs it over hundreds of generated sessions.
//!
//! # Where the numbers come from is recorded
//!
//! A boundary that a `Sample.Clock` reading landed on exactly is
//! [`Provenance::Measured`]. A boundary between two readings is
//! [`Provenance::Interpolated`] — and that assumes constant power across the
//! gap, which a tapering charge curve does not deliver. The error lands on
//! whichever supplier held the boundary, so the assumption travels with the
//! number rather than being forgotten.
//!
//! # Quarter hours and daylight saving
//!
//! A quarter hour here is an instant plus fifteen minutes of real time. Every
//! civil UTC offset in the world is a whole number of quarter hours, so a UTC
//! quarter-hour boundary is a local quarter-hour boundary everywhere — and the
//! 92- and 100-slot days of a clock change are simply days with fewer or more
//! instants in them. Nothing here counts to 96, and there is no DST branch.

use emob_core::{Direction, Energy};
use rust_decimal::Decimal;

use crate::meter::{MeterSeries, ReadingContext};

/// Fifteen minutes of real time, starting at an instant on a quarter-hour
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct QuarterHour(time::OffsetDateTime);

impl QuarterHour {
    /// Fifteen minutes, in seconds.
    pub const SECONDS: i64 = 900;

    /// The quarter hour that contains `at`.
    #[must_use]
    pub fn containing(at: time::OffsetDateTime) -> Self {
        // Truncate towards negative infinity on the UTC timeline, so a session
        // that starts at 10:07:30 belongs to the slot beginning 10:00 whatever
        // the local offset happens to be.
        let unix = at.unix_timestamp();
        let floored = unix.div_euclid(Self::SECONDS) * Self::SECONDS;
        Self(
            time::OffsetDateTime::from_unix_timestamp(floored)
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
                .to_offset(at.offset()),
        )
    }

    /// The instant this quarter hour begins.
    #[must_use]
    pub const fn start(self) -> time::OffsetDateTime {
        self.0
    }

    /// The instant it ends, which is the next one's start.
    #[must_use]
    pub fn end(self) -> time::OffsetDateTime {
        self.0 + time::Duration::seconds(Self::SECONDS)
    }

    /// The quarter hour after this one.
    #[must_use]
    pub fn next(self) -> Self {
        Self(self.end())
    }
}

impl core::fmt::Display for QuarterHour {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}",
            self.0.year(),
            u8::from(self.0.month()),
            self.0.day(),
            self.0.hour(),
            self.0.minute()
        )
    }
}

/// Where a number came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Provenance {
    /// A meter reading landed on this instant. The station measured it.
    Measured,
    /// Derived by assuming constant power between two readings.
    ///
    /// Which a tapering charge curve does not deliver: a car at 80 % state of
    /// charge draws far less at the end of a gap than at its start, so a
    /// straight line over-allocates to the later side. The error is bounded by
    /// the gap, which is why `AlignedDataInterval = 900` matters.
    Interpolated,
}

impl Provenance {
    /// The weaker of two provenances — what a slot inherits from its two
    /// boundaries.
    #[must_use]
    pub const fn weaker(self, other: Self) -> Self {
        match (self, other) {
            (Self::Measured, Self::Measured) => Self::Measured,
            _ => Self::Interpolated,
        }
    }
}

/// One quarter hour's worth of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Slot {
    /// Which quarter hour.
    pub quarter_hour: QuarterHour,
    /// How much energy the session moved inside it.
    pub energy: Energy,
    /// Which way.
    pub direction: Direction,
    /// How the number was arrived at.
    pub provenance: Provenance,
}

/// A session, split across the quarter hours it touched.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SessionSplit {
    /// The slots, in time order, with no gaps.
    pub slots: Vec<Slot>,
    /// The session total the split was taken from.
    pub total: Energy,
    /// The direction of every slot.
    pub direction: Direction,
}

impl SessionSplit {
    /// Whether the slots add up to the total, exactly.
    ///
    /// Always true by construction; asserted anyway, because a conservation
    /// property that is never checked is a conservation property that quietly
    /// stops holding.
    #[must_use]
    pub fn conserves(&self) -> bool {
        self.slots.iter().map(|s| s.energy).sum::<Energy>() == self.total
    }

    /// The slots whose energy was interpolated rather than measured.
    pub fn interpolated(&self) -> impl Iterator<Item = &Slot> {
        self.slots
            .iter()
            .filter(|s| s.provenance == Provenance::Interpolated)
    }

    /// Whether every slot was measured at both its boundaries.
    ///
    /// The question a settlement process should ask before treating the split
    /// as authoritative.
    #[must_use]
    pub fn fully_measured(&self) -> bool {
        self.slots
            .iter()
            .all(|s| s.provenance == Provenance::Measured)
    }
}

/// Split a session's meter series across quarter hours.
///
/// ```
/// use emob_session::{MeterReading, MeterSeries, ReadingContext, split};
/// use emob_core::{Direction, Energy};
/// use rust_decimal::Decimal;
/// use std::str::FromStr;
/// use time::macros::datetime;
///
/// let kwh = |s: &str| Energy::from_kwh(Decimal::from_str(s).unwrap()).unwrap();
/// let series = MeterSeries::new(Direction::Import, vec![
///     MeterReading::new(datetime!(2026-01-02 10:00 +1), kwh("100.000"), Direction::Import, ReadingContext::TransactionBegin),
///     MeterReading::new(datetime!(2026-01-02 10:15 +1), kwh("110.000"), Direction::Import, ReadingContext::SampleClock),
///     MeterReading::new(datetime!(2026-01-02 10:30 +1), kwh("118.000"), Direction::Import, ReadingContext::TransactionEnd),
/// ])?;
///
/// let split = split::into_quarter_hours(&series)?;
/// assert_eq!(split.slots.len(), 2);
/// assert_eq!(split.slots[0].energy.to_string(), "10.000 kWh");
/// assert_eq!(split.slots[1].energy.to_string(), "8.000 kWh");
/// assert!(split.conserves());
/// assert!(split.fully_measured());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// [`SplitError`] when the series cannot be split — a zero-length session, or
/// one so long that splitting it is a corruption rather than a charge.
pub fn into_quarter_hours(series: &MeterSeries) -> Result<SessionSplit, SplitError> {
    let start = series.first().at;
    let end = series.last().at;

    if end <= start {
        return Err(SplitError::NoDuration { at: start });
    }

    // A corruption guard, not a regulatory bound: a "session" spanning more
    // than a year is a clock fault or a parsing error, and allocating it would
    // build 35 000 slots before anybody noticed.
    let span = end - start;
    if span > time::Duration::days(366) {
        return Err(SplitError::ImplausiblyLong {
            days: span.whole_days(),
        });
    }

    // The boundaries: the session start, every quarter-hour boundary strictly
    // inside it, and the session end. Note that the first and last are *not*
    // quarter-hour boundaries in general — a session starting at 10:07 has its
    // first slot run 10:07 to 10:15, and that slot is still reported under the
    // quarter hour beginning 10:00, because that is the settlement period the
    // energy belongs to.
    let mut boundaries: Vec<time::OffsetDateTime> = vec![start];
    let mut cursor = QuarterHour::containing(start).next();
    while cursor.start() < end {
        boundaries.push(cursor.start());
        cursor = cursor.next();
    }
    boundaries.push(end);

    // Each boundary's cumulative register value, computed exactly once. This is
    // what makes the sum telescope.
    let cumulative: Vec<(Decimal, Provenance)> = boundaries
        .iter()
        .map(|&at| cumulative_at(series, at))
        .collect();

    let mut slots = Vec::with_capacity(boundaries.len().saturating_sub(1));
    for i in 0..boundaries.len() - 1 {
        let (from, from_prov) = cumulative[i];
        let (to, to_prov) = cumulative[i + 1];
        let energy = Energy::from_kwh(to - from).map_err(|_| SplitError::NonMonotonic {
            at: boundaries[i + 1],
        })?;
        slots.push(Slot {
            quarter_hour: QuarterHour::containing(boundaries[i]),
            energy,
            direction: series.direction(),
            provenance: from_prov.weaker(to_prov),
        });
    }

    let total = series
        .total()
        .map_err(|_| SplitError::NonMonotonic { at: end })?;

    let split = SessionSplit {
        slots,
        total,
        direction: series.direction(),
    };

    // The invariant this whole module exists for. If it ever fails, the bug is
    // here and not in the caller, so it is worth saying so loudly rather than
    // returning a number nobody can use.
    debug_assert!(
        split.conserves(),
        "the telescoping sum must equal the total exactly"
    );

    Ok(split)
}

/// The register's cumulative value at an instant, and how it was arrived at.
fn cumulative_at(series: &MeterSeries, at: time::OffsetDateTime) -> (Decimal, Provenance) {
    let readings = series.readings();

    // Before the first or after the last reading: the endpoints. Both are
    // measured — they *are* readings.
    if at <= readings[0].at {
        return (readings[0].register.kwh(), Provenance::Measured);
    }
    let last = &readings[readings.len() - 1];
    if at >= last.at {
        return (last.register.kwh(), Provenance::Measured);
    }

    for pair in readings.windows(2) {
        let (before, after) = (&pair[0], &pair[1]);
        if at == before.at {
            return (before.register.kwh(), measured_if_useful(before.context));
        }
        if at > before.at && at < after.at {
            // Linear interpolation: constant power across the gap.
            let gap = (after.at - before.at).whole_seconds();
            let offset = (at - before.at).whole_seconds();
            if gap <= 0 {
                return (before.register.kwh(), Provenance::Interpolated);
            }
            let delta = after.register.kwh() - before.register.kwh();
            let fraction = Decimal::from(offset) / Decimal::from(gap);
            return (
                before.register.kwh() + delta * fraction,
                Provenance::Interpolated,
            );
        }
    }

    (last.register.kwh(), Provenance::Measured)
}

/// A reading that lands on a boundary counts as measuring it only when it was
/// *meant* to.
///
/// A `Sample.Periodic` reading that happens to fall on `:15:00` measured the
/// register, certainly — but the station chose that instant for its own
/// reasons, and treating the coincidence as a clock-aligned measurement would
/// report a settlement as authoritative on a day when the phase drifted.
/// Transaction boundaries count because the session genuinely begins and ends
/// there.
const fn measured_if_useful(context: ReadingContext) -> Provenance {
    if context.is_clock_aligned() || context.is_transaction_boundary() {
        Provenance::Measured
    } else {
        Provenance::Interpolated
    }
}

/// Why a session could not be split.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SplitError {
    /// The first and last readings are at the same instant.
    #[error("the session has no duration: every reading is at {at}")]
    NoDuration {
        /// The instant.
        at: time::OffsetDateTime,
    },

    /// The session spans more than a year.
    #[error("the session spans {days} days, which is a clock fault rather than a charge")]
    ImplausiblyLong {
        /// How many days.
        days: i64,
    },

    /// A cumulative value decreased.
    #[error("the cumulative register decreased at {at}")]
    NonMonotonic {
        /// Where.
        at: time::OffsetDateTime,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meter::MeterReading;
    use std::str::FromStr;
    use time::macros::datetime;

    fn kwh(s: &str) -> Energy {
        Energy::from_kwh(Decimal::from_str(s).unwrap()).unwrap()
    }

    fn at(minute: i64) -> time::OffsetDateTime {
        datetime!(2026-01-02 10:00 +1) + time::Duration::minutes(minute)
    }

    fn series(points: &[(i64, &str, ReadingContext)]) -> MeterSeries {
        MeterSeries::new(
            Direction::Import,
            points
                .iter()
                .map(|&(m, v, c)| MeterReading::new(at(m), kwh(v), Direction::Import, c))
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn a_session_on_the_boundaries_is_fully_measured() {
        let s = series(&[
            (0, "100.000", ReadingContext::TransactionBegin),
            (15, "110.000", ReadingContext::SampleClock),
            (30, "118.000", ReadingContext::TransactionEnd),
        ]);
        let split = into_quarter_hours(&s).unwrap();

        assert_eq!(split.slots.len(), 2);
        assert_eq!(split.slots[0].energy.to_string(), "10.000 kWh");
        assert_eq!(split.slots[1].energy.to_string(), "8.000 kWh");
        assert!(split.fully_measured());
        assert!(split.conserves());
    }

    #[test]
    fn a_session_starting_mid_slot_reports_under_the_slot_it_belongs_to() {
        // 10:07 → 10:23 touches the quarter hours beginning 10:00 and 10:15.
        let s = series(&[
            (7, "100.0", ReadingContext::TransactionBegin),
            (23, "108.0", ReadingContext::TransactionEnd),
        ]);
        let split = into_quarter_hours(&s).unwrap();

        assert_eq!(split.slots.len(), 2);
        assert_eq!(split.slots[0].quarter_hour.to_string(), "2026-01-02T10:00");
        assert_eq!(split.slots[1].quarter_hour.to_string(), "2026-01-02T10:15");
        assert!(split.conserves());
        // Nothing measured 10:15, so both slots inherit the interpolation.
        assert!(!split.fully_measured());
        assert_eq!(split.interpolated().count(), 2);
    }

    #[test]
    fn conservation_survives_a_ratio_that_does_not_divide() {
        // 10:07 → 10:23 is 16 minutes; the boundary at 10:15 is 8/16 of the
        // way. Fine. Now make it awkward: 10:01 → 10:22, boundary at 10:15 is
        // 14/21 = 2/3 of the way, and 7 kWh × 2/3 is a repeating decimal.
        let s = series(&[
            (1, "0", ReadingContext::TransactionBegin),
            (22, "7", ReadingContext::TransactionEnd),
        ]);
        let split = into_quarter_hours(&s).unwrap();

        assert_eq!(split.slots.len(), 2);
        assert!(
            split.conserves(),
            "slots {:?} must sum to {}",
            split.slots.iter().map(|s| s.energy).collect::<Vec<_>>(),
            split.total
        );
        assert_eq!(
            split.slots.iter().map(|s| s.energy).sum::<Energy>(),
            kwh("7")
        );
    }

    #[test]
    fn a_long_session_conserves_across_many_slots() {
        // Six hours, readings only at the ends: 24 slots, every interior
        // boundary interpolated, and the sum still exact.
        let s = series(&[
            (0, "0", ReadingContext::TransactionBegin),
            (360, "123.456", ReadingContext::TransactionEnd),
        ]);
        let split = into_quarter_hours(&s).unwrap();

        assert_eq!(split.slots.len(), 24);
        assert!(split.conserves());
        assert_eq!(
            split.slots.iter().map(|s| s.energy).sum::<Energy>(),
            kwh("123.456")
        );
    }

    #[test]
    fn conservation_holds_over_many_generated_sessions() {
        // A property test without a property-testing dependency: a spread of
        // start offsets, durations and totals, each of which produces a
        // different set of awkward ratios.
        let mut checked = 0;
        for start_min in [0_i64, 1, 4, 7, 13, 14] {
            for duration in [3_i64, 16, 21, 47, 90, 133] {
                for total in ["0.001", "7", "13.37", "123.456789"] {
                    let s = series(&[
                        (start_min, "0", ReadingContext::TransactionBegin),
                        (start_min + duration, total, ReadingContext::TransactionEnd),
                    ]);
                    let split = into_quarter_hours(&s).unwrap();
                    assert!(
                        split.conserves(),
                        "start={start_min} duration={duration} total={total}"
                    );
                    assert_eq!(
                        split.slots.iter().map(|x| x.energy).sum::<Energy>(),
                        kwh(total)
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 144);
    }

    #[test]
    fn a_periodic_sample_on_a_boundary_is_still_interpolated() {
        // The station chose that instant for its own reasons. Treating the
        // coincidence as a clock-aligned measurement would report a settlement
        // as authoritative on a day the phase drifted.
        let s = series(&[
            (0, "100", ReadingContext::TransactionBegin),
            (15, "110", ReadingContext::SamplePeriodic),
            (30, "118", ReadingContext::TransactionEnd),
        ]);
        let split = into_quarter_hours(&s).unwrap();
        assert!(!split.fully_measured());

        // …and with the same reading marked Sample.Clock, it is.
        let s = series(&[
            (0, "100", ReadingContext::TransactionBegin),
            (15, "110", ReadingContext::SampleClock),
            (30, "118", ReadingContext::TransactionEnd),
        ]);
        assert!(into_quarter_hours(&s).unwrap().fully_measured());
    }

    #[test]
    fn a_session_inside_one_quarter_hour_is_one_slot() {
        let s = series(&[
            (1, "100", ReadingContext::TransactionBegin),
            (9, "104", ReadingContext::TransactionEnd),
        ]);
        let split = into_quarter_hours(&s).unwrap();
        assert_eq!(split.slots.len(), 1);
        assert_eq!(split.slots[0].energy.to_string(), "4 kWh");
        assert!(split.conserves());
    }

    #[test]
    fn a_session_with_no_duration_is_refused() {
        let s = MeterSeries::new(
            Direction::Import,
            vec![
                MeterReading::new(
                    at(0),
                    kwh("100"),
                    Direction::Import,
                    ReadingContext::TransactionBegin,
                ),
                MeterReading::new(
                    at(0),
                    kwh("100"),
                    Direction::Import,
                    ReadingContext::TransactionEnd,
                ),
            ],
        )
        .unwrap();
        assert!(matches!(
            into_quarter_hours(&s),
            Err(SplitError::NoDuration { .. })
        ));
    }

    #[test]
    fn an_implausibly_long_session_is_refused_before_it_allocates() {
        let s = MeterSeries::new(
            Direction::Import,
            vec![
                MeterReading::new(
                    at(0),
                    kwh("0"),
                    Direction::Import,
                    ReadingContext::TransactionBegin,
                ),
                MeterReading::new(
                    at(0) + time::Duration::days(400),
                    kwh("1"),
                    Direction::Import,
                    ReadingContext::TransactionEnd,
                ),
            ],
        )
        .unwrap();
        assert!(matches!(
            into_quarter_hours(&s),
            Err(SplitError::ImplausiblyLong { .. })
        ));
    }

    #[test]
    fn quarter_hours_align_on_the_utc_grid() {
        for (minute, expected) in [
            (0, 0),
            (7, 0),
            (14, 0),
            (15, 15),
            (29, 15),
            (30, 30),
            (59, 45),
        ] {
            let q = QuarterHour::containing(at(minute));
            assert_eq!(q.start().minute(), expected, "minute {minute}");
        }
    }

    #[test]
    fn a_clock_change_needs_no_special_case() {
        // Europe/Berlin springs forward at 02:00 local on 2026-03-29. The slots
        // either side are fifteen minutes of real time each; the local clock
        // jumping is not the split's business.
        let before = datetime!(2026-03-29 01:45:00 +1);
        let after = before + time::Duration::minutes(30);
        let s = MeterSeries::new(
            Direction::Import,
            vec![
                MeterReading::new(
                    before,
                    kwh("0"),
                    Direction::Import,
                    ReadingContext::TransactionBegin,
                ),
                MeterReading::new(
                    after,
                    kwh("6"),
                    Direction::Import,
                    ReadingContext::TransactionEnd,
                ),
            ],
        )
        .unwrap();
        let split = into_quarter_hours(&s).unwrap();
        assert_eq!(split.slots.len(), 2, "two quarter hours of real time");
        assert!(split.conserves());
    }

    #[test]
    fn provenance_takes_the_weaker_of_two_boundaries() {
        assert_eq!(
            Provenance::Measured.weaker(Provenance::Measured),
            Provenance::Measured
        );
        assert_eq!(
            Provenance::Measured.weaker(Provenance::Interpolated),
            Provenance::Interpolated
        );
        assert_eq!(
            Provenance::Interpolated.weaker(Provenance::Measured),
            Provenance::Interpolated
        );
    }

    #[test]
    fn export_sessions_split_the_same_way_and_stay_export() {
        let s = MeterSeries::new(
            Direction::Export,
            vec![
                MeterReading::new(
                    at(0),
                    kwh("0"),
                    Direction::Export,
                    ReadingContext::TransactionBegin,
                ),
                MeterReading::new(
                    at(30),
                    kwh("5"),
                    Direction::Export,
                    ReadingContext::TransactionEnd,
                ),
            ],
        )
        .unwrap();
        let split = into_quarter_hours(&s).unwrap();
        assert_eq!(split.direction, Direction::Export);
        assert!(split.slots.iter().all(|x| x.direction == Direction::Export));
        assert!(split.conserves());
    }
}
