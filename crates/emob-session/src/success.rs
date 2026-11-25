//! The charging success rate: sessions that reached a billable record over
//! sessions attempted, per charge point and for the estate.
//!
//! # Why from the ledger, and why here
//!
//! The industry reports *uptime* (~99 %) and measures *success* (80–86 %), and
//! the gap between the two is every driver who plugged in and drove away
//! without a charge. The figure is only worth publishing if nobody can argue
//! with its numerator, so it is counted from the same facts that bill: an
//! attempt succeeds when its record **settles**, and a record settles once —
//! the count of successes is the count of records an invoice is built from.
//!
//! It is a fold rather than a query because a daemon's state is the fold of
//! its journal: [`Tally::record`] is called once per fact, in order, and a
//! replay of the same journal is the same tally.
//!
//! # What a failure was
//!
//! Four causes, because each has a different owner:
//!
//! | Cause | Whose fix |
//! |---|---|
//! | [`Cause::Authorisation`] | the provider's, or the driver's card |
//! | [`Cause::Evidence`] | the meter's or the provisioning's — `[MessEG §33]` refused the energy |
//! | [`Cause::StationFault`] | the station vendor's |
//! | [`Cause::Cancelled`] | nobody's — the driver changed their mind |
//!
//! [`cause_of_refusal`] decides which, from what the station said when it
//! ended the session and whether anything was measured.
//!
//! # Per day, because a bad morning hides behind a good year
//!
//! An all-time rate moves by a hundredth of a percent on the day half the
//! estate fails. [`Daily`] keeps one [`Tally`] per calendar day and answers any
//! window of days — "today" and "the last seven" on the operator page. The day
//! is the caller's: the fold dates each attempt in the estate's zone from the
//! instant it was decided, so this module reads no clock.

use std::collections::BTreeMap;

use rust_decimal::{Decimal, RoundingStrategy};

use crate::EndReason;

/// Why an attempt did not reach a billable record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Cause {
    /// The provider did not let the token charge.
    Authorisation,
    /// The evidence chain refused the record.
    Evidence,
    /// The station ended the session with a fault or a power loss.
    StationFault,
    /// The driver ended it before anything was measured.
    Cancelled,
}

impl Cause {
    /// Every cause, in the order a page lists them.
    pub const ALL: [Self; 4] = [
        Self::Authorisation,
        Self::Evidence,
        Self::StationFault,
        Self::Cancelled,
    ];
}

/// The cause of a session that ended and reached no billable record.
///
/// `stopped` is what the station said when it ended the session;
/// `nothing_measured` is whether no signed reading moved — the session carried
/// none, or the chain's only finding was that there was no energy to bill.
///
/// The station's own word wins when it names a fault: a meter that lost power
/// mid-session leaves a chain with a hole in it, and calling that an evidence
/// failure sends the operator to the provisioning instead of to the vendor. A
/// driver who stopped before anything flowed cancelled; a driver who stopped
/// after energy flowed and still got no record is an evidence failure, because
/// something was measured and could not be billed.
#[must_use]
pub fn cause_of_refusal(stopped: Option<EndReason>, nothing_measured: bool) -> Cause {
    match stopped {
        Some(EndReason::Fault | EndReason::PowerLoss) => Cause::StationFault,
        Some(EndReason::DeAuthorized) => Cause::Authorisation,
        Some(EndReason::Local | EndReason::EvDisconnected | EndReason::Remote)
            if nothing_measured =>
        {
            Cause::Cancelled
        }
        _ => Cause::Evidence,
    }
}

/// How one attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// Its record settled.
    Billed,
    /// It reached no billable record.
    Failed(Cause),
}

/// Attempts and their outcomes, for one point or for the estate.
///
/// `billed + Σ failed = attempted`, by construction: `Rate::add` is the only
/// way in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Rate {
    /// Sessions attempted.
    pub attempted: u64,
    /// Sessions whose record settled.
    pub billed: u64,
    /// The failures, by cause; a cause with none is absent.
    pub failed: BTreeMap<Cause, u64>,
}

impl Rate {
    fn merge(&mut self, other: &Self) {
        self.attempted += other.attempted;
        self.billed += other.billed;
        for (cause, count) in &other.failed {
            *self.failed.entry(*cause).or_default() += count;
        }
    }

    fn add(&mut self, ending: Ending) {
        self.attempted += 1;
        match ending {
            Ending::Billed => self.billed += 1,
            Ending::Failed(cause) => *self.failed.entry(cause).or_default() += 1,
        }
    }

    /// The failures of one cause.
    #[must_use]
    pub fn failed_by(&self, cause: Cause) -> u64 {
        self.failed.get(&cause).copied().unwrap_or_default()
    }

    /// The rate in percent, rounded half-even to one decimal — `None` before
    /// the first attempt, because a point nobody tried is not 100 % reliable.
    ///
    /// Exact: two integers into a decimal, never a float. The page shows the
    /// rounded figure **beside** `billed / attempted`, so the rounding is
    /// stated rather than discovered.
    #[must_use]
    pub fn percent(&self) -> Option<Decimal> {
        if self.attempted == 0 {
            return None;
        }
        let ratio =
            Decimal::from(self.billed) * Decimal::ONE_HUNDRED / Decimal::from(self.attempted);
        Some(ratio.round_dp_with_strategy(1, RoundingStrategy::MidpointNearestEven))
    }
}

/// The success rate of an estate, per charge point.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Tally {
    /// The whole estate.
    pub estate: Rate,
    /// Each point, by the identity it connects under.
    pub points: BTreeMap<String, Rate>,
}

impl Tally {
    /// Record one attempt at `point`.
    pub fn record(&mut self, point: &str, ending: Ending) {
        self.estate.add(ending);
        self.points.entry(point.to_owned()).or_default().add(ending);
    }

    /// Add another tally to this one — two days into a window.
    pub fn merge(&mut self, other: &Self) {
        self.estate.merge(&other.estate);
        for (point, rate) in &other.points {
            self.points.entry(point.clone()).or_default().merge(rate);
        }
    }

    /// The `n` points that fail most: lowest rate first, then most attempts,
    /// then by name. A point with no failure is not on the list.
    #[must_use]
    pub fn worst(&self, n: usize) -> Vec<(&str, &Rate)> {
        let mut failing: Vec<(&str, &Rate)> = self
            .points
            .iter()
            .filter(|(_, rate)| rate.billed < rate.attempted)
            .map(|(point, rate)| (point.as_str(), rate))
            .collect();
        // billed/attempted ascending, compared exactly as a cross product.
        failing.sort_by(|(a_name, a), (b_name, b)| {
            (u128::from(a.billed) * u128::from(b.attempted))
                .cmp(&(u128::from(b.billed) * u128::from(a.attempted)))
                .then(b.attempted.cmp(&a.attempted))
                .then(a_name.cmp(b_name))
        });
        failing.truncate(n);
        failing
    }
}

/// One [`Tally`] per calendar day, as far back as [`Daily::RETENTION_DAYS`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Daily {
    days: BTreeMap<time::Date, Tally>,
}

impl Daily {
    /// How many days before the newest are kept; older ones are dropped as a
    /// later day is recorded, so the memory is bounded by the calendar.
    pub const RETENTION_DAYS: i64 = 400;

    /// Record one attempt at `point` on `day`.
    pub fn record(&mut self, day: time::Date, point: &str, ending: Ending) {
        self.days.entry(day).or_default().record(point, ending);
        let first_kept = day.saturating_sub(time::Duration::days(Self::RETENTION_DAYS));
        if self
            .days
            .first_key_value()
            .is_some_and(|(oldest, _)| *oldest < first_kept)
        {
            self.days = self.days.split_off(&first_kept);
        }
    }

    /// The retained tally for one local date, including its estate total.
    #[must_use]
    pub fn on(&self, day: time::Date) -> Option<&Tally> {
        self.days.get(&day)
    }

    /// The attempts of the days `from..=to`, summed; empty when none.
    #[must_use]
    pub fn window(&self, from: time::Date, to: time::Date) -> Tally {
        let mut sum = Tally::default();
        if from <= to {
            for tally in self.days.range(from..=to).map(|(_, tally)| tally) {
                sum.merge(tally);
            }
        }
        sum
    }

    /// How many days hold an attempt.
    #[must_use]
    pub fn len(&self) -> usize {
        self.days.len()
    }

    /// Whether no attempt was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.days.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_station_naming_a_fault_wins_over_the_chain() {
        assert_eq!(
            cause_of_refusal(Some(EndReason::PowerLoss), false),
            Cause::StationFault
        );
        assert_eq!(
            cause_of_refusal(Some(EndReason::Fault), true),
            Cause::StationFault
        );
        assert_eq!(
            cause_of_refusal(Some(EndReason::DeAuthorized), false),
            Cause::Authorisation
        );
    }

    #[test]
    fn a_driver_who_stopped_before_anything_flowed_cancelled_and_after_it_did_not() {
        assert_eq!(
            cause_of_refusal(Some(EndReason::Local), true),
            Cause::Cancelled
        );
        assert_eq!(
            cause_of_refusal(Some(EndReason::EvDisconnected), true),
            Cause::Cancelled
        );
        // Energy flowed and was refused: that is the evidence, whoever stopped.
        assert_eq!(
            cause_of_refusal(Some(EndReason::Local), false),
            Cause::Evidence
        );
        assert_eq!(cause_of_refusal(None, true), Cause::Evidence);
        assert_eq!(
            cause_of_refusal(Some(EndReason::EvFull), true),
            Cause::Evidence
        );
    }

    #[test]
    fn the_counts_add_up_and_the_percent_is_exact() {
        let mut tally = Tally::default();
        tally.record("CP-1", Ending::Billed);
        tally.record("CP-1", Ending::Failed(Cause::Evidence));
        tally.record("CP-2", Ending::Failed(Cause::Authorisation));
        assert_eq!(tally.estate.attempted, 3);
        assert_eq!(tally.estate.billed, 1);
        assert_eq!(tally.estate.failed_by(Cause::Evidence), 1);
        assert_eq!(
            tally.estate.billed
                + Cause::ALL
                    .iter()
                    .map(|c| tally.estate.failed_by(*c))
                    .sum::<u64>(),
            tally.estate.attempted
        );
        assert_eq!(tally.estate.percent(), Some(Decimal::new(333, 1)));
        assert_eq!(tally.points["CP-2"].percent(), Some(Decimal::ZERO));
    }

    #[test]
    fn nobody_tried_is_not_a_hundred_percent() {
        assert_eq!(Rate::default().percent(), None);
    }

    #[test]
    fn half_even_at_the_midpoint() {
        // 1/16 = 6.25 % → 6.2; 3/16 = 18.75 % → 18.8.
        let rate = |billed, attempted| Rate {
            attempted,
            billed,
            failed: BTreeMap::new(),
        };
        assert_eq!(rate(1, 16).percent(), Some(Decimal::new(62, 1)));
        assert_eq!(rate(3, 16).percent(), Some(Decimal::new(188, 1)));
    }

    #[test]
    fn the_worst_points_are_the_lowest_rate_and_a_perfect_one_is_not_listed() {
        let mut tally = Tally::default();
        for _ in 0..4 {
            tally.record("good", Ending::Billed);
        }
        tally.record("half", Ending::Billed);
        tally.record("half", Ending::Failed(Cause::Cancelled));
        tally.record("never", Ending::Failed(Cause::StationFault));
        let worst: Vec<&str> = tally.worst(5).into_iter().map(|(name, _)| name).collect();
        assert_eq!(worst, ["never", "half"]);
    }

    #[test]
    fn a_window_sums_its_days_and_nothing_outside_them() {
        use time::macros::date;
        let mut daily = Daily::default();
        daily.record(date!(2026 - 11 - 11), "CP-1", Ending::Billed);
        daily.record(
            date!(2026 - 11 - 12),
            "CP-1",
            Ending::Failed(Cause::Evidence),
        );
        daily.record(date!(2026 - 11 - 18), "CP-2", Ending::Billed);
        let today = daily.window(date!(2026 - 11 - 18), date!(2026 - 11 - 18));
        assert_eq!((today.estate.attempted, today.estate.billed), (1, 1));
        // Seven days ending today: the 12th is in, the 11th is not.
        let week = daily.window(date!(2026 - 11 - 12), date!(2026 - 11 - 18));
        assert_eq!((week.estate.attempted, week.estate.billed), (2, 1));
        assert_eq!(week.points["CP-1"].failed_by(Cause::Evidence), 1);
        assert_eq!(week.worst(3)[0].0, "CP-1");
        // A quiet day is no attempts, not yesterday's figure.
        let quiet = daily.window(date!(2026 - 11 - 19), date!(2026 - 11 - 19));
        assert_eq!(quiet.estate.percent(), None);
    }

    #[test]
    fn days_past_the_retention_are_dropped_as_a_later_one_is_recorded() {
        use time::macros::date;
        let mut daily = Daily::default();
        daily.record(date!(2025 - 01 - 01), "CP-1", Ending::Billed);
        daily.record(date!(2025 - 06 - 01), "CP-1", Ending::Billed);
        assert_eq!(daily.len(), 2);
        daily.record(date!(2026 - 03 - 01), "CP-1", Ending::Billed);
        assert_eq!(daily.len(), 2, "1 January 2025 is past 400 days");
        assert_eq!(
            daily
                .window(date!(2025 - 01 - 01), date!(2025 - 01 - 01))
                .estate
                .attempted,
            0
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn a_tally_travels_as_counts_keyed_by_cause() {
        let mut tally = Tally::default();
        tally.record("CP-1", Ending::Failed(Cause::StationFault));
        let json = serde_json::to_value(&tally).unwrap();
        assert_eq!(json["estate"]["failed"]["station_fault"], 1);
        let back: Tally = serde_json::from_value(json).unwrap();
        assert_eq!(back, tally);
    }
}
