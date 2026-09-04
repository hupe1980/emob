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
//! # The grid is not the only thing that cuts a session
//!
//! A quarter hour is where the *energy* settles. It is not where the *price*
//! changes: `[AFIR Art. 5(4)]` lets a fast charger add an occupancy fee per
//! minute for the time a vehicle is connected and **not** charging, and a
//! vehicle stops charging when it stops charging, not at `:15:00`. A slot that
//! ran from 10:15 to 10:30 with the charge finishing at 10:20 is ten minutes of
//! occupancy and five minutes of charging, and one flag cannot say so.
//!
//! So [`into_periods`] takes extra cut instants beside the grid — the session's
//! own state changes, for the caller that has them — and every one of them is
//! just another boundary in the same telescoping sum. Conservation is
//! unaffected: interior boundaries cancel whatever they were rounded to,
//! wherever they fall. [`Session::split`] passes the session's history, so a
//! CDR built from it prices each minute at the rate that minute earned.
//!
//! [`Session::split`]: crate::Session::split
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
//! # …and constant power means constant power *while charging*
//!
//! A straight line between two readings is the honest guess across a gap the
//! session says nothing about. It is a wrong guess across a gap the session
//! says something about: a transaction that opens `EVConnected`, starts
//! charging thirty seconds later and sends its next meter value at the quarter
//! hour — the ordinary OCPP 2.0.1 shape — has a register that did not move for
//! those thirty seconds, and a line drawn from the opening reading attributes
//! thirty seconds' worth of energy to an interval the operator's own state
//! machine calls suspended. The CDR builder then refuses the record for a
//! contradiction the arithmetic invented.
//!
//! So [`into_periods`] takes the session's **idle intervals** beside its cuts,
//! and interpolates over *charging time only*: the register is held flat across
//! an idle interval and the gap's energy is spread over the seconds that remain.
//! Where a gap has no charging time at all and the register moved anyway, the
//! line is drawn across the whole gap and the contradiction is left standing for
//! the builder to report — it is now a real one.
//!
//! # Quarter hours and daylight saving
//!
//! A quarter hour here is an instant plus fifteen minutes of real time. Every
//! civil UTC offset in the world is a whole number of quarter hours, so a UTC
//! quarter-hour boundary is a local quarter-hour boundary everywhere — and the
//! 92- and 100-slot days of a clock change are simply days with fewer or more
//! instants in them. Nothing here counts to 96, and there is no DST branch.

use emob_core::{Direction, Energy, QuarterHour, apportion};
use rust_decimal::Decimal;

use crate::meter::{MeterSeries, ReadingContext};

/// Where a number came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Provenance {
    /// A meter reading landed on this instant. The station measured it.
    Measured,
    /// The register was **held** at a measured reading's value across an
    /// interval the session says nothing flowed in.
    ///
    /// The value is exact, on the session's own account: between the reading
    /// and this instant the vehicle was connected and not charging, so the
    /// register cannot have moved. What it rests on is the operator's state
    /// machine rather than a second reading — which is why it is a third answer
    /// rather than [`Self::Measured`], and why a settlement may still treat the
    /// number as authoritative where it may not treat an interpolated one so.
    Held,
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
            (Self::Interpolated, _) | (_, Self::Interpolated) => Self::Interpolated,
            _ => Self::Held,
        }
    }

    /// Whether the number this provenance describes is exact — measured, or
    /// held at a measurement — rather than assumed.
    #[must_use]
    pub const fn is_exact(self) -> bool {
        !matches!(self, Self::Interpolated)
    }

    /// The provenance of a boundary whose value was carried unchanged from a
    /// reading: a held measurement, or an interpolation that was already one.
    const fn held_from(reading: Self) -> Self {
        match reading {
            Self::Measured | Self::Held => Self::Held,
            Self::Interpolated => Self::Interpolated,
        }
    }
}

/// One slice of a session, inside one quarter hour.
///
/// The settlement slot and the measured window are separate fields, and that is
/// the whole point of the type. A session whose readings run 10:07 to 10:23 has
/// its first slot reported under the quarter hour beginning **10:00** — that is
/// the settlement period the energy belongs to `[A6 §IV.1]` — while the energy
/// itself was measured over **10:07 to 10:15**. Both statements are true and
/// they are different instants, and a consumer that reconstructs one from the
/// other has to guess.
///
/// One quarter hour may hold **more than one slot**, whenever a cut passed to
/// [`into_periods`] falls inside it — a session that stops charging at 10:20
/// yields a charging slice and an occupancy slice, both under the quarter hour
/// beginning 10:15. [`SessionSplit::market_series`] sums them back together for
/// the market side, which settles per period and not per slice.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Slot {
    /// Which quarter hour this energy settles in.
    pub quarter_hour: QuarterHour,
    /// The first instant the meter series covers inside that quarter hour.
    ///
    /// Equal to [`QuarterHour::start`] for every slot but the first, and to the
    /// first reading's instant for that one.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub from: time::OffsetDateTime,
    /// The last instant it covers — [`QuarterHour::end`] except in the final
    /// slot, which stops at the last reading.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub to: time::OffsetDateTime,
    /// How much energy the session moved inside it.
    pub energy: Energy,
    /// Which way.
    pub direction: Direction,
    /// How the number was arrived at.
    pub provenance: Provenance,
}

impl Slot {
    /// How long the measured window lasted.
    #[must_use]
    pub fn duration(&self) -> time::Duration {
        self.to - self.from
    }

    /// Whether the slot's readings cover its whole quarter hour.
    ///
    /// False for the first and last slot of a session that began or ended
    /// mid-quarter — which is most of them — and for every slice of a quarter
    /// hour a cut divided.
    #[must_use]
    pub fn covers_the_whole_quarter_hour(&self) -> bool {
        self.from == self.quarter_hour.start() && self.to == self.quarter_hour.end()
    }
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

    /// Whether every slot's energy is exact — measured at both its boundaries,
    /// or held at a measurement across an interval the session says nothing
    /// flowed in — with nothing interpolated anywhere.
    ///
    /// The question a settlement process should ask before treating the split
    /// as authoritative.
    #[must_use]
    pub fn fully_measured(&self) -> bool {
        self.slots.iter().all(|s| s.provenance.is_exact())
    }

    /// The series in the form the market side reads it: each period labelled by
    /// its **end**.
    ///
    /// The one conversion that separates this workspace's grid from a German
    /// load profile. Internally a quarter hour is named by its start, because
    /// that is the only spelling in which `containing()` is a truncation;
    /// `[PTB-A 50.7 §3.1.7.2]` labels a Messperiode by its end, and so do
    /// MSCONS and `mako-emob`. Mixing the two shifts every slot by fifteen
    /// minutes — an error that sums to zero across the session and is wrong for
    /// every individual balance group `[A6 §IV.1]`.
    ///
    /// So the conversion happens once, here, rather than in each adapter that
    /// needs it.
    ///
    /// Slices of one quarter hour are summed back together. A cut at a state
    /// change divides a slot for *pricing* — an occupancy fee is a price per
    /// minute `[AFIR Art. 5(4)]` — and the market side settles a whole
    /// Messperiode against one balance group, so handing it two entries for one
    /// timestamp would be a file no `mako-emob` allocation can read.
    #[must_use]
    pub fn market_series(&self) -> Vec<(time::OffsetDateTime, Energy)> {
        let mut series: Vec<(time::OffsetDateTime, Energy)> = Vec::new();
        for slot in &self.slots {
            let at = slot.quarter_hour.metering_timestamp();
            match series.last_mut() {
                Some((last, energy)) if *last == at => *energy += slot.energy,
                _ => series.push((at, slot.energy)),
            }
        }
        series
    }
}

/// Split a session's meter series across quarter hours.
///
/// ```
/// use emob_session::{MeterReading, MeterSeries, ReadingContext, split};
/// use emob_core::{Direction, Energy, QuarterHour};
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
    into_periods(series, &[], &[])
}

/// An interval `[from, to)` in which the session says no energy flowed.
///
/// What [`into_periods`] holds the register flat across. The session's own
/// history is the source — [`crate::Session::idle_intervals`] derives them —
/// and a meter series never produces one, because a meter cannot tell a taper
/// from a suspension.
pub type Idle = (time::OffsetDateTime, time::OffsetDateTime);

/// Split a session's meter series across quarter hours, cutting also at every
/// instant in `cuts` and interpolating only across the time outside `idle`.
///
/// The grid says where the *energy* settles `[A6 §IV.1]`; `cuts` say where
/// anything else about the session changed. The caller with the session's own
/// state machine passes its transition instants, and each resulting slice
/// carries one answer to "was the vehicle charging here" instead of one answer
/// for a quarter hour that held two.
///
/// `idle` is the same history read the other way: the intervals in which the
/// session says nothing flowed. A boundary that falls inside one takes the
/// register value at the interval's start, and a gap between two readings
/// spreads its energy over the seconds that were **not** idle — because a
/// straight line across a suspension attributes energy to an interval the
/// operator's own record says moved none, and the CDR builder then refuses a
/// contradiction the arithmetic invented. See the module documentation.
///
/// Cuts outside the series, and cuts that land on a boundary the grid already
/// produced, cost one comparison and change nothing; so do idle intervals
/// outside the series. Conservation is unaffected whatever they are: every
/// interior boundary still appears once positive and once negative in the
/// telescoping sum.
///
/// ```
/// use emob_session::{MeterReading, MeterSeries, ReadingContext, split};
/// use emob_core::{Direction, Energy};
/// use rust_decimal::Decimal;
/// use time::macros::datetime;
///
/// # let kwh = |s: &str| Energy::from_kwh(<Decimal as std::str::FromStr>::from_str(s).unwrap()).unwrap();
/// let series = MeterSeries::new(Direction::Import, vec![
///     MeterReading::new(datetime!(2026-01-02 10:00 +1), kwh("100.000"), Direction::Import, ReadingContext::TransactionBegin),
///     MeterReading::new(datetime!(2026-01-02 10:30 +1), kwh("110.000"), Direction::Import, ReadingContext::TransactionEnd),
/// ])?;
///
/// // The charge finished at 10:20, in the middle of the second quarter hour,
/// // and the car sat there until 10:30.
/// let stopped = datetime!(2026-01-02 10:20 +1);
/// let split = split::into_periods(&series, &[stopped], &[(stopped, datetime!(2026-01-02 10:30 +1))])?;
/// assert_eq!(split.slots.len(), 3, "10:00–10:15, 10:15–10:20, 10:20–10:30");
/// assert!(split.slots[2].energy.is_zero(), "nothing flowed while it sat there");
/// assert!(split.conserves());
/// // …and the market side still sees two Messperioden.
/// assert_eq!(split.market_series().len(), 2);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// The same as [`into_quarter_hours`].
pub fn into_periods(
    series: &MeterSeries,
    cuts: &[time::OffsetDateTime],
    idle: &[Idle],
) -> Result<SessionSplit, SplitError> {
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
    // inside it, every cut strictly inside it, and the session end. Note that
    // the first and last are *not* quarter-hour boundaries in general — a
    // session starting at 10:07 has its first slot run 10:07 to 10:15, and that
    // slot is still reported under the quarter hour beginning 10:00, because
    // that is the settlement period the energy belongs to.
    let mut boundaries: Vec<time::OffsetDateTime> = vec![start];
    let mut cursor = QuarterHour::containing(start).next();
    while cursor.start() < end {
        boundaries.push(cursor.start());
        cursor = cursor.next();
    }
    boundaries.extend(cuts.iter().copied().filter(|&at| at > start && at < end));
    boundaries.push(end);
    // A cut that coincides with a grid boundary is the same boundary, and a
    // duplicate would produce a slice of no duration and no energy.
    boundaries.sort_unstable();
    boundaries.dedup();

    // Each boundary's cumulative register value, computed exactly once. This is
    // what makes the sum telescope.
    let idle = normalised_idle(idle, start, end);
    let cumulative = cumulative_along(series, &boundaries, &idle);

    let mut slots = Vec::with_capacity(boundaries.len().saturating_sub(1));
    for i in 0..boundaries.len() - 1 {
        let (from, from_prov) = cumulative[i];
        let (to, to_prov) = cumulative[i + 1];
        let energy = Energy::from_kwh(to - from).map_err(|_| SplitError::NonMonotonic {
            at: boundaries[i + 1],
        })?;
        slots.push(Slot {
            quarter_hour: QuarterHour::containing(boundaries[i]),
            // The window the readings actually cover, which is the boundary
            // pair this slot's energy was taken between — never the quarter
            // hour clamped to the session, because the two differ whenever the
            // session's own window is wider than its meter series.
            from: boundaries[i],
            to: boundaries[i + 1],
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

/// The register's cumulative value at every boundary, and how each was arrived
/// at.
///
/// `boundaries` is ascending, and so are the readings, so the two are walked
/// **together** rather than the readings being searched once per boundary. A
/// month-long session sampled every five minutes has thousands of each, and
/// the nested form is quadratic in a function that runs before anything is
/// billed.
fn cumulative_along(
    series: &MeterSeries,
    boundaries: &[time::OffsetDateTime],
    idle: &[Idle],
) -> Vec<(Decimal, Provenance)> {
    let readings = series.readings();
    let last = &readings[readings.len() - 1];
    // The index of the last reading at or before the current boundary. It only
    // ever moves forward, which is what makes the whole walk linear.
    let mut index = 0usize;

    boundaries
        .iter()
        .map(|&at| {
            // Before the first or after the last reading: the endpoints. Both
            // are measured — they *are* readings.
            if at <= readings[0].at {
                return (readings[0].register.kwh(), Provenance::Measured);
            }
            if at >= last.at {
                return (last.register.kwh(), Provenance::Measured);
            }
            while index + 1 < readings.len() && readings[index + 1].at <= at {
                index += 1;
            }
            let before = &readings[index];
            if at == before.at {
                return (before.register.kwh(), measured_if_useful(before.context));
            }
            let after = &readings[index + 1];

            // Linear interpolation: constant power across the gap — across the
            // part of it the session was charging in. The register is held
            // flat over an idle interval, so the gap's energy is spread over
            // the seconds that remain. A gap with no charging time at all in
            // which the register nonetheless moved is a contradiction between
            // the meter and the state machine; the line is then drawn across
            // the whole gap, and the CDR builder reports the disagreement.
            let (gap, offset) = {
                let active_gap = charging_seconds(before.at, after.at, idle);
                if active_gap > 0 {
                    let active_offset = charging_seconds(before.at, at, idle);
                    // No charging time between a reading and this boundary:
                    // the register cannot have moved, so the value is the
                    // reading's — held, not interpolated — from whichever
                    // side the idle stretch touches.
                    if active_offset == 0 {
                        return (
                            before.register.kwh(),
                            Provenance::held_from(measured_if_useful(before.context)),
                        );
                    }
                    if active_offset == active_gap {
                        return (
                            after.register.kwh(),
                            Provenance::held_from(measured_if_useful(after.context)),
                        );
                    }
                    (active_gap, active_offset)
                } else {
                    (
                        (after.at - before.at).whole_seconds(),
                        (at - before.at).whole_seconds(),
                    )
                }
            };
            if gap <= 0 {
                return (before.register.kwh(), Provenance::Interpolated);
            }
            let delta = after.register.kwh() - before.register.kwh();
            // The same arithmetic the rating engine places a tariff threshold
            // with, through the same function: multiply before dividing, and
            // quote the result at a scale a *sum* of such values can carry, so
            // the telescoping identity this module rests on holds as written.
            (
                apportion(
                    before.register.kwh(),
                    delta,
                    offset.unsigned_abs(),
                    gap.unsigned_abs(),
                ),
                Provenance::Interpolated,
            )
        })
        .collect()
}

/// The idle intervals clipped to the series, sorted, and merged where they
/// touch or overlap — so [`charging_seconds`] can subtract them without
/// counting a second twice.
fn normalised_idle(
    idle: &[Idle],
    start: time::OffsetDateTime,
    end: time::OffsetDateTime,
) -> Vec<Idle> {
    let mut clipped: Vec<Idle> = idle
        .iter()
        .map(|&(from, to)| (from.max(start), to.min(end)))
        .filter(|(from, to)| to > from)
        .collect();
    clipped.sort_unstable();
    let mut merged: Vec<Idle> = Vec::with_capacity(clipped.len());
    for (from, to) in clipped {
        match merged.last_mut() {
            Some((_, last_to)) if from <= *last_to => *last_to = (*last_to).max(to),
            _ => merged.push((from, to)),
        }
    }
    merged
}

/// The whole seconds of `[from, to)` that lie outside every idle interval.
///
/// `idle` is [`normalised_idle`]'s output: disjoint and ascending, so each
/// interval's overlap with the window is subtracted exactly once.
fn charging_seconds(from: time::OffsetDateTime, to: time::OffsetDateTime, idle: &[Idle]) -> i64 {
    let total = (to - from).whole_seconds();
    let idle_inside: i64 = idle
        .iter()
        .map(|&(a, b)| (b.min(to) - a.max(from)).whole_seconds().max(0))
        .sum();
    total - idle_inside
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
        assert!(
            split.slots.iter().all(Slot::covers_the_whole_quarter_hour),
            "a session on the boundaries covers every slot it touches"
        );
    }

    #[test]
    fn an_interpolation_that_does_not_terminate_still_conserves() {
        // The boundary at 10:15 falls 840/1260 of the way through 10:01 →
        // 10:22, and `7 × 840 / 1260` is 4.666… — a ratio no decimal states.
        // The value is the workspace's one apportionment, quoted at the scale
        // a *sum* of such values can carry: without that floor two of them
        // spend the whole 96-bit mantissa on their fractions and cannot be
        // added exactly, which is a conservation check failing in the last
        // place of the assertion that exists to prove there is none.
        let s = series(&[
            (1, "0", ReadingContext::TransactionBegin),
            (22, "7", ReadingContext::TransactionEnd),
        ]);
        let split = into_quarter_hours(&s).unwrap();

        assert_eq!(
            split.slots[0].energy.kwh(),
            emob_core::apportion(Decimal::ZERO, Decimal::from(7), 840, 1260)
        );
        assert_eq!(split.slots[0].energy.kwh().to_string(), "4.666666666667");
        assert!(split.conserves(), "and the pieces still telescope exactly");
        assert!(!split.fully_measured());
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

        // The settlement slot and the measured window are different instants,
        // and the slot carries both rather than leaving a consumer to guess.
        assert_eq!(split.slots[0].from, at(7), "the readings begin at 10:07");
        assert_eq!(split.slots[0].to, at(15));
        assert_eq!(split.slots[0].duration(), time::Duration::minutes(8));
        assert!(!split.slots[0].covers_the_whole_quarter_hour());
        assert_eq!(split.slots[1].from, at(15));
        assert_eq!(split.slots[1].to, at(23));

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
    fn the_market_series_labels_each_period_by_its_end() {
        // `[PTB-A 50.7 §3.1.7.2]`: "der Zeitstempel 00:15:00 [gehört] zur
        // ersten Messperiode eines Tages". Handing `mako-emob` a series
        // labelled by the start would shift every slot by a quarter hour — an
        // error that sums to zero and is wrong for every balance group.
        let s = series(&[
            (0, "100.000", ReadingContext::TransactionBegin),
            (15, "110.000", ReadingContext::SampleClock),
            (30, "118.000", ReadingContext::TransactionEnd),
        ]);
        let split = into_quarter_hours(&s).unwrap();

        assert_eq!(
            split.market_series(),
            vec![(at(15), kwh("10.000")), (at(30), kwh("8.000"))],
            "the first period ends at 10:15 and is labelled 10:15"
        );
        assert_eq!(
            split.slots[0].quarter_hour.start(),
            at(0),
            "…and starts at 10:00"
        );

        // The energy is the same energy either way round.
        assert_eq!(
            split
                .market_series()
                .iter()
                .map(|(_, e)| *e)
                .sum::<Energy>(),
            split.total
        );
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
    fn a_cut_divides_a_quarter_hour_without_moving_any_energy() {
        // The charge finishes at 10:20, in the middle of the second quarter
        // hour. Split on the grid alone, that whole quarter hour carries one
        // answer to "was the vehicle charging"; cut, it carries two.
        let s = series(&[
            (0, "100.000", ReadingContext::TransactionBegin),
            (20, "110.000", ReadingContext::InterruptionBegin),
            (45, "110.000", ReadingContext::TransactionEnd),
        ]);
        let split = into_periods(&s, &[at(20)], &[]).unwrap();

        let windows: Vec<(i64, i64)> = split
            .slots
            .iter()
            .map(|slot| {
                (
                    (slot.from - at(0)).whole_minutes(),
                    (slot.to - at(0)).whole_minutes(),
                )
            })
            .collect();
        assert_eq!(windows, vec![(0, 15), (15, 20), (20, 30), (30, 45)]);
        assert_eq!(split.slots[1].quarter_hour, split.slots[2].quarter_hour);
        assert!(split.conserves());
        assert_eq!(
            split.slots.iter().map(|x| x.energy).sum::<Energy>(),
            kwh("10.000")
        );
    }

    #[test]
    fn the_market_series_sums_the_slices_of_one_messperiode() {
        // A cut divides a slot for *pricing*; the market side settles a whole
        // Messperiode against one balance group `[A6 §IV.1]`, so two entries
        // for one timestamp would be a file no allocation can read.
        let s = series(&[
            (0, "100.000", ReadingContext::TransactionBegin),
            (30, "110.000", ReadingContext::TransactionEnd),
        ]);
        let split = into_periods(&s, &[at(20), at(25)], &[]).unwrap();
        assert_eq!(split.slots.len(), 4, "10:00, 10:15, 10:20, 10:25");

        let market = split.market_series();
        assert_eq!(
            market,
            vec![(at(15), kwh("5.000")), (at(30), kwh("5.000"))],
            "two quarter hours, whatever the pricing cuts did inside them"
        );
        assert_eq!(market.iter().map(|(_, e)| *e).sum::<Energy>(), split.total);
    }

    #[test]
    fn a_cut_outside_the_series_or_on_a_boundary_changes_nothing() {
        let s = series(&[
            (0, "100.000", ReadingContext::TransactionBegin),
            (30, "118.000", ReadingContext::TransactionEnd),
        ]);
        let plain = into_quarter_hours(&s).unwrap();
        // Before the start, after the end, on the start, on the end, and on a
        // grid boundary the split already produced.
        let cut = into_periods(&s, &[at(-5), at(0), at(15), at(30), at(99)], &[]).unwrap();
        assert_eq!(cut, plain);
        // …and so does an idle interval that lies outside the series.
        let idle = into_periods(&s, &[], &[(at(-30), at(-5)), (at(30), at(60))]).unwrap();
        assert_eq!(idle, plain);
    }

    #[test]
    fn energy_is_interpolated_over_charging_time_only() {
        // The ordinary OCPP 2.0.1 shape: the transaction opens `EVConnected`
        // with a `Transaction.Begin` reading, starts charging thirty seconds
        // later, and sends its next meter value at the quarter hour. Drawn as
        // one straight line, the first thirty seconds get 1/30 of the gap's
        // energy — attributed to an interval the session says moved none.
        let begin = at(0);
        let charging_from = at(0) + time::Duration::seconds(30);
        let s = series(&[
            (0, "100.000", ReadingContext::TransactionBegin),
            (15, "105.000", ReadingContext::SampleClock),
        ]);

        let naive = into_periods(&s, &[charging_from], &[]).unwrap();
        assert!(
            !naive.slots[0].energy.is_zero(),
            "a straight line invents energy in the suspended interval: {}",
            naive.slots[0].energy
        );

        let split = into_periods(&s, &[charging_from], &[(begin, charging_from)]).unwrap();
        assert_eq!(split.slots.len(), 2);
        assert!(
            split.slots[0].energy.is_zero(),
            "the register is held flat while nothing flows"
        );
        assert_eq!(split.slots[1].energy, kwh("5.000"));
        assert!(split.conserves());
        // …and both numbers are exact rather than assumed: the boundary at
        // 10:00:30 is the opening reading's value, held.
        assert_eq!(split.slots[0].provenance, Provenance::Held);
        assert_eq!(split.slots[1].provenance, Provenance::Held);
        assert!(split.fully_measured(), "nothing was interpolated");
        assert_eq!(split.interpolated().count(), 0);
    }

    #[test]
    fn a_held_value_is_only_as_good_as_the_reading_it_is_held_from() {
        // Held from a `Sample.Periodic` reading — a coincidence rather than a
        // measurement — is still an assumption.
        let charging_from = at(15) + time::Duration::seconds(30);
        let s = series(&[
            (0, "100.000", ReadingContext::TransactionBegin),
            (15, "105.000", ReadingContext::SamplePeriodic),
            (30, "110.000", ReadingContext::TransactionEnd),
        ]);
        let split = into_periods(&s, &[charging_from], &[(at(15), charging_from)]).unwrap();
        assert_eq!(split.slots[1].energy, Energy::ZERO);
        assert_eq!(split.slots[1].provenance, Provenance::Interpolated);
        assert!(!split.fully_measured());

        // Provenance orders as measured > held > interpolated.
        assert_eq!(
            Provenance::Measured.weaker(Provenance::Held),
            Provenance::Held
        );
        assert_eq!(Provenance::Held.weaker(Provenance::Held), Provenance::Held);
        assert_eq!(
            Provenance::Held.weaker(Provenance::Interpolated),
            Provenance::Interpolated
        );
        assert!(Provenance::Held.is_exact());
        assert!(!Provenance::Interpolated.is_exact());
    }

    #[test]
    fn an_idle_interval_in_the_middle_of_a_gap_moves_energy_to_the_charging_sides() {
        // Readings at 10:00 and 10:30, 12 kWh between them; suspended from
        // 10:10 to 10:20. Twenty charging minutes carry the twelve
        // kilowatt-hours: six before the pause, none during it — on either
        // side of the 10:15 grid boundary — and six after.
        let s = series(&[
            (0, "0", ReadingContext::TransactionBegin),
            (30, "12", ReadingContext::TransactionEnd),
        ]);
        let split = into_periods(&s, &[at(10), at(20)], &[(at(10), at(20))]).unwrap();
        let energies: Vec<String> = split
            .slots
            .iter()
            .map(|slot| slot.energy.kwh().normalize().to_string())
            .collect();
        assert_eq!(energies, ["6", "0", "0", "6"]);
        assert!(split.conserves());
    }

    #[test]
    fn a_register_that_moves_across_a_wholly_idle_gap_is_left_for_the_builder() {
        // The session says nothing flowed between the two readings and the
        // meter says 5 kWh did. There is no charging time to spread it over,
        // so the line is drawn across the whole gap — the contradiction is
        // real, and `emob-cdr` is where it is reported.
        let s = series(&[
            (0, "100", ReadingContext::TransactionBegin),
            (30, "105", ReadingContext::TransactionEnd),
        ]);
        let split = into_periods(&s, &[], &[(at(0), at(30))]).unwrap();
        assert_eq!(split.slots.len(), 2);
        assert_eq!(split.slots[0].energy.kwh().normalize().to_string(), "2.5");
        assert!(split.conserves());
    }

    #[test]
    fn overlapping_and_touching_idle_intervals_count_each_second_once() {
        let s = series(&[
            (0, "0", ReadingContext::TransactionBegin),
            (30, "12", ReadingContext::TransactionEnd),
        ]);
        // 10:10–10:20 stated three times over, in pieces that overlap and touch.
        let messy = [(at(10), at(16)), (at(14), at(18)), (at(18), at(20))];
        let clean = [(at(10), at(20))];
        assert_eq!(
            into_periods(&s, &[at(10), at(20)], &messy).unwrap(),
            into_periods(&s, &[at(10), at(20)], &clean).unwrap()
        );
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
