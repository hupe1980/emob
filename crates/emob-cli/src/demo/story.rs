//! The morning the demo plays, and the one clock it is played on.
//!
//! The story is fixed — which station, which card, when, for how long, and the
//! one tampered record — and the seed decides the rest: the meters' keys and
//! registers and every session's energy. Its events are walked in simulated
//! order, so what reaches the wire, and therefore every 1.6 transaction id, CDR
//! key and invoice number, is the same at any `--speed`.

use std::sync::Mutex;
use std::time::Instant;

use emob_core::Energy;
use emob_sim::{ChargedSession, Fault, Rng, SessionPlan, VirtualStation};
use rust_decimal::Decimal;

use super::estate::{BLOCKED_CARD, GOOD_CARD};

/// One beat of the story.
#[derive(Debug, Clone, Copy)]
pub struct Beat {
    /// Minutes after the story's start.
    pub at: i64,
    /// Index into `estate::CHARGE_POINTS`.
    pub station: usize,
    /// The card presented.
    pub card: &'static str,
    /// How long the vehicle stays, in minutes.
    pub minutes: i64,
    /// What goes wrong, if anything.
    pub fault: Option<Fault>,
}

const fn beat(at: i64, station: usize, minutes: i64) -> Beat {
    Beat {
        at,
        station,
        card: GOOD_CARD,
        minutes,
        fault: None,
    }
}

/// The morning (data-model.md § Story).
pub const STORY: [Beat; 9] = [
    beat(0, 0, 22),
    Beat {
        fault: Some(Fault::TamperedValue),
        ..beat(3, 1, 41)
    },
    beat(9, 2, 28),
    Beat {
        card: BLOCKED_CARD,
        ..beat(24, 0, 5)
    },
    beat(31, 0, 34),
    beat(50, 1, 45),
    beat(64, 2, 24),
    beat(72, 0, 30),
    beat(98, 1, 38),
];

/// How many sessions the story bills: every one with the good card and no
/// fault.
#[must_use]
pub fn billable() -> usize {
    STORY
        .iter()
        .filter(|beat| beat.card == GOOD_CARD && beat.fault.is_none())
        .count()
}

/// One session, planned and charged.
#[derive(Debug, Clone)]
pub struct Session {
    /// Its beat.
    pub beat: Beat,
    /// The transaction id a 2.x station gives it.
    pub label: String,
    /// What the meter signed, and the events a station sends for it.
    pub charged: ChargedSession,
}

/// The story's sessions, charged on `meters` in order.
#[must_use]
pub fn sessions(
    seed: u64,
    start: time::OffsetDateTime,
    meters: &mut [VirtualStation],
) -> Vec<Session> {
    let mut rng = Rng::stream(seed, "story");
    let mut count = [0_u32; 3];
    STORY
        .iter()
        .map(|beat| {
            let started_at = start + time::Duration::minutes(beat.at);
            let plan = SessionPlan {
                started_at,
                ended_at: started_at + time::Duration::minutes(beat.minutes),
                energy: Energy::from_wh(Decimal::from(rng.between(6_000, 40_000)))
                    .expect("a positive draw"),
                // Quarter-hour readings between begin and end, as a real
                // meter signs them: whole watt-hours.
                clock_aligned: true,
                connected_before_charging: time::Duration::seconds(30),
            };
            let faults: Vec<Fault> = beat.fault.into_iter().collect();
            let charged = meters[beat.station].charge(&plan, &faults);
            count[beat.station] += 1;
            Session {
                beat: *beat,
                label: format!("tx-{:03}", count[beat.station]),
                charged,
            }
        })
        .collect()
}

/// Every event of the story as `(instant, session, event)`, in the order a
/// wire sees them.
#[must_use]
pub fn timeline(sessions: &[Session]) -> Vec<(time::OffsetDateTime, usize, usize)> {
    let mut all: Vec<_> = sessions
        .iter()
        .enumerate()
        .flat_map(|(s, session)| {
            session
                .charged
                .events
                .iter()
                .enumerate()
                .map(move |(e, event)| (event.at, s, e))
        })
        .collect();
    all.sort();
    all
}

/// How fast simulated time runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    /// This many simulated seconds per real one.
    Times(u32),
    /// As fast as the estate answers.
    Max,
}

/// The one clock the stations stamp and Heute reads: the start plus real
/// elapsed time × speed, set exactly at each beat, frozen when the story ends.
pub struct Clock {
    held: Mutex<(time::OffsetDateTime, Instant, bool)>,
    speed: Speed,
}

impl Clock {
    /// A clock at `start`.
    #[must_use]
    pub fn new(start: time::OffsetDateTime, speed: Speed) -> Self {
        Self {
            held: Mutex::new((start, Instant::now(), false)),
            speed,
        }
    }

    /// The simulated now.
    pub fn now(&self) -> time::OffsetDateTime {
        let (at, since, frozen) = *self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match self.speed {
            Speed::Times(times) if !frozen => {
                let real =
                    time::Duration::try_from(since.elapsed()).unwrap_or(time::Duration::ZERO);
                at + real * times
            }
            _ => at,
        }
    }

    /// Wait until simulated time reaches `at`, then set it there exactly.
    pub async fn reach(&self, at: time::OffsetDateTime) {
        if let Speed::Times(times) = self.speed {
            let ahead = at - self.now();
            if ahead.is_positive() {
                let real = ahead / times;
                tokio::time::sleep(real.try_into().unwrap_or_default()).await;
            }
        }
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if at > held.0 || self.speed == Speed::Max {
            *held = (at.max(held.0), Instant::now(), held.2);
        }
    }

    /// Stop the clock where it is.
    pub fn freeze(&self) {
        let now = self.now();
        *self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = (now, Instant::now(), true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::estate;

    fn keys(seed: u64) -> Vec<(String, Vec<String>)> {
        let start = time::macros::datetime!(2026-06-01 08:00 UTC);
        sessions(seed, start, &mut estate::meters(seed))
            .into_iter()
            .map(|s| (s.label, s.charged.records))
            .collect()
    }

    #[test]
    fn one_seed_is_one_story() {
        assert_eq!(
            keys(2026),
            keys(2026),
            "US1.4: the same seed, the same records"
        );
        assert_ne!(keys(2026), keys(7), "and another seed another one");
    }

    #[test]
    fn the_story_bills_seven_refuses_one_and_blocks_one() {
        assert_eq!(billable(), 7);
        assert_eq!(STORY.iter().filter(|b| b.fault.is_some()).count(), 1);
        assert_eq!(STORY.iter().filter(|b| b.card == BLOCKED_CARD).count(), 1);
    }

    #[test]
    fn a_station_never_charges_two_vehicles_at_once() {
        for station in 0..3 {
            let mut beats: Vec<_> = STORY.iter().filter(|b| b.station == station).collect();
            beats.sort_by_key(|b| b.at);
            for pair in beats.windows(2) {
                assert!(pair[0].at + pair[0].minutes <= pair[1].at, "{pair:?}");
            }
        }
    }

    #[test]
    fn a_frozen_clock_stands_still() {
        let clock = Clock::new(
            time::macros::datetime!(2026-06-01 08:00 UTC),
            Speed::Times(60),
        );
        clock.freeze();
        let at = clock.now();
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(clock.now(), at);
    }
}
