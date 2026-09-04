//! The session itself, and the states it may legally be in.
//!
//! A session is not a row that gets updated. It is a sequence of events with an
//! order the protocol guarantees — authorised, plugged, charging, suspended,
//! ended — and a state machine that refuses the transitions the protocol
//! forbids. Modelling it as mutable fields is how a session ends twice, or
//! delivers energy after it stopped, and both of those reach an invoice.

use emob_core::{Direction, Energy, EvseId, SessionId};

use crate::auth::Authorization;
use crate::meter::MeterSeries;
use crate::split::{SessionSplit, SplitError, into_periods};

/// Where a session is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum SessionState {
    /// Authorised, but no energy has moved. A reservation, or a cable not yet
    /// plugged in.
    Pending,
    /// Energy is moving.
    Charging,
    /// Plugged in and authorised, but not charging — the car is full, or a
    /// smart-charging profile is holding it at zero. Distinct from
    /// [`Self::Charging`] because parking time is priced differently from
    /// energy `[AFIR Art. 5(4)]`.
    Suspended,
    /// Over. Nothing further may be added.
    Ended,
}

impl SessionState {
    /// Whether the session has finished.
    #[must_use]
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Ended)
    }

    /// Whether a transition to `next` is one the protocol allows.
    ///
    /// Written as one arm per rule rather than as the shortest expression that
    /// computes the same booleans. The table *is* the specification of a
    /// session's life, and a reader checking it against OCPP needs to find each
    /// rule where it lives.
    #[must_use]
    #[allow(
        clippy::match_same_arms,
        reason = "one arm per protocol rule; collapsing them by return value \
                  would make the transition table unreadable"
    )]
    pub const fn can_transition_to(self, next: Self) -> bool {
        match (self, next) {
            // Nothing follows the end.
            (Self::Ended, _) => false,
            // A session may start charging, be held at zero, or be abandoned.
            (Self::Pending, Self::Charging | Self::Suspended | Self::Ended) => true,
            // Charging may pause or finish; it may not go back to pending,
            // because energy has already moved.
            (Self::Charging, Self::Suspended | Self::Ended) => true,
            // A suspended session may resume or finish.
            (Self::Suspended, Self::Charging | Self::Ended) => true,
            _ => false,
        }
    }
}

/// Why a session ended.
///
/// Worth recording separately from the state because a dispute usually turns on
/// it: a session the operator cut off is a different conversation from one the
/// driver ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum EndReason {
    /// The driver stopped it at the point.
    Local,
    /// The backend stopped it.
    Remote,
    /// The cable was unplugged.
    EvDisconnected,
    /// The vehicle reported itself full.
    EvFull,
    /// Authorisation was withdrawn.
    DeAuthorized,
    /// A fault.
    Fault,
    /// Power was lost.
    PowerLoss,
    /// Something else the station named.
    Other,
}

/// One state the session was in, and when it entered it.
///
/// The history exists because "suspended" is not a status light, it is an
/// interval that gets priced: `[AFIR Art. 5(4)]` lets a fast charger add an
/// occupancy fee per minute for the time the vehicle is connected and not
/// charging. A state field with no timestamp can say the session *is*
/// suspended and never say for how long, which is precisely the number the
/// article turns on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StateChange {
    /// The state entered.
    pub state: SessionState,
    /// When.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub at: time::OffsetDateTime,
}

/// A charging session.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Session {
    /// Which session.
    pub id: SessionId,
    /// Where it happened.
    pub evse_id: EvseId,
    /// How it was authorised, and for whom.
    pub authorization: Authorization,
    /// When it began.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub started_at: time::OffsetDateTime,
    /// When it ended, once it has.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339::option"))]
    pub ended_at: Option<time::OffsetDateTime>,
    /// Why it ended.
    pub end_reason: Option<EndReason>,
    /// Every state the session has been in, in order, starting with
    /// [`SessionState::Pending`] at [`Self::started_at`].
    history: Vec<StateChange>,
    /// The energy it moved, by direction.
    ///
    /// A vector rather than one series because a bidirectional session has two
    /// registers counting two quantities, and they never net.
    pub series: Vec<MeterSeries>,
}

impl Session {
    /// Open a session in [`SessionState::Pending`].
    #[must_use]
    pub fn open(
        id: SessionId,
        evse_id: EvseId,
        authorization: Authorization,
        started_at: time::OffsetDateTime,
    ) -> Self {
        Self {
            id,
            evse_id,
            authorization,
            started_at,
            ended_at: None,
            end_reason: None,
            history: vec![StateChange {
                state: SessionState::Pending,
                at: started_at,
            }],
            series: Vec::new(),
        }
    }

    /// Where the session is now.
    #[must_use]
    pub fn state(&self) -> SessionState {
        // `open` seeds the history and nothing empties it.
        self.history[self.history.len() - 1].state
    }

    /// Every state it has been in, in order.
    #[must_use]
    pub fn history(&self) -> &[StateChange] {
        &self.history
    }

    /// The state the session was in at an instant.
    ///
    /// `None` before it started.
    #[must_use]
    pub fn state_at(&self, at: time::OffsetDateTime) -> Option<SessionState> {
        self.history
            .iter()
            .rev()
            .find(|change| change.at <= at)
            .map(|change| change.state)
    }

    /// Whether the session was suspended for the whole of `[from, to)` — the
    /// interval an occupancy fee may be charged for `[AFIR Art. 5(4)]`.
    #[must_use]
    pub fn suspended_throughout(
        &self,
        from: time::OffsetDateTime,
        to: time::OffsetDateTime,
    ) -> bool {
        if to <= from {
            return false;
        }
        if self.state_at(from) != Some(SessionState::Suspended) {
            return false;
        }
        // No transition may land strictly inside the interval.
        !self
            .history
            .iter()
            .any(|change| change.at > from && change.at < to)
    }

    /// Whether the session was charging for the whole of `[from, to)`.
    ///
    /// # Not the negation of [`Self::suspended_throughout`]
    ///
    /// There are four states and two of them are neither: a [`Pending`] session
    /// is authorised with no energy moving, and an [`Ended`] one is over. Both
    /// are "connected and not charging" — which is what `[AFIR Art. 5(4)]`'s
    /// occupancy fee prices — and neither is suspended.
    ///
    /// This is the question `emob-cdr` asks of every period it prices, metered
    /// or not: a period is charging when the operator's own record says it was,
    /// and occupancy otherwise. Asking `!suspended_throughout` instead reads
    /// "the session never said suspended" as "the vehicle was charging", which
    /// is an absence standing in for a claim — and bills the minute a car sat
    /// `EVConnected` before its charge began as charging time.
    ///
    /// [`Pending`]: SessionState::Pending
    /// [`Ended`]: SessionState::Ended
    #[must_use]
    pub fn charging_throughout(
        &self,
        from: time::OffsetDateTime,
        to: time::OffsetDateTime,
    ) -> bool {
        if to <= from {
            return false;
        }
        if self.state_at(from) != Some(SessionState::Charging) {
            return false;
        }
        !self
            .history
            .iter()
            .any(|change| change.at > from && change.at < to)
    }

    /// Move to a new state at an instant.
    ///
    /// # Errors
    ///
    /// [`SessionError::IllegalTransition`] when the protocol forbids it —
    /// above all, anything at all after the session ended.
    /// [`SessionError::TimeWentBackwards`] when the instant precedes the last
    /// transition, because a history that is not ordered cannot be read as
    /// intervals.
    pub fn transition_to(
        &mut self,
        next: SessionState,
        at: time::OffsetDateTime,
    ) -> Result<(), SessionError> {
        let current = self.state();
        if !current.can_transition_to(next) {
            return Err(SessionError::IllegalTransition {
                from: current,
                to: next,
            });
        }
        let last = self.history[self.history.len() - 1].at;
        if at < last {
            return Err(SessionError::TimeWentBackwards { previous: last, at });
        }
        self.history.push(StateChange { state: next, at });
        Ok(())
    }

    /// End the session.
    ///
    /// # Errors
    ///
    /// [`SessionError::IllegalTransition`] when it has already ended,
    /// [`SessionError::EndsBeforeItStarts`] when the end precedes the start, or
    /// [`SessionError::TimeWentBackwards`] when it precedes the last
    /// transition.
    pub fn end(&mut self, at: time::OffsetDateTime, reason: EndReason) -> Result<(), SessionError> {
        if at < self.started_at {
            return Err(SessionError::EndsBeforeItStarts {
                started_at: self.started_at,
                ended_at: at,
            });
        }
        self.transition_to(SessionState::Ended, at)?;
        self.ended_at = Some(at);
        self.end_reason = Some(reason);
        Ok(())
    }

    /// Attach a meter series.
    ///
    /// # Errors
    ///
    /// [`SessionError::AlreadyEnded`] once the session is final — a series
    /// arriving after the end is either a late duplicate or a different
    /// session, and guessing which is how energy gets double-billed.
    /// [`SessionError::DuplicateDirection`] when a direction already has one.
    pub fn attach_series(&mut self, series: MeterSeries) -> Result<(), SessionError> {
        if self.state().is_final() {
            return Err(SessionError::AlreadyEnded);
        }
        if self
            .series
            .iter()
            .any(|s| s.direction() == series.direction())
        {
            return Err(SessionError::DuplicateDirection {
                direction: series.direction(),
            });
        }
        self.series.push(series);
        Ok(())
    }

    /// The series for a direction.
    #[must_use]
    pub fn series_for(&self, direction: Direction) -> Option<&MeterSeries> {
        self.series.iter().find(|s| s.direction() == direction)
    }

    /// Total energy in a direction.
    ///
    /// `None` when the session recorded nothing in that direction — which is
    /// not the same as zero, and a caller that treats it as zero will invoice a
    /// session it has no readings for.
    #[must_use]
    pub fn total(&self, direction: Direction) -> Option<Energy> {
        self.series_for(direction).and_then(|s| s.total().ok())
    }

    /// How long the session lasted, once it has ended.
    #[must_use]
    pub fn duration(&self) -> Option<time::Duration> {
        self.ended_at.map(|end| end - self.started_at)
    }

    /// The intervals in which this session says no energy flowed — every
    /// stretch of its history in a state other than [`SessionState::Charging`],
    /// as `[from, to)` pairs in time order.
    ///
    /// A `Pending` session is authorised with nothing flowing, a `Suspended`
    /// one is held at zero, and an `Ended` one is over; none of them moves the
    /// register. The last interval — the time after the end, or the current
    /// state of a session still running — is open-ended.
    ///
    /// This is what [`Self::split`] hands the interpolation, so a gap between
    /// two readings spreads its energy over the seconds the session was
    /// charging rather than in a straight line across a suspension. A meter
    /// cannot tell a taper from a pause; the state machine can, and it is the
    /// operator's own record of which was which.
    #[must_use]
    pub fn idle_intervals(&self) -> Vec<(time::OffsetDateTime, time::OffsetDateTime)> {
        let mut idle = Vec::new();
        for (index, change) in self.history.iter().enumerate() {
            if change.state == SessionState::Charging {
                continue;
            }
            // The last entry — `Ended`, or whatever state a running session
            // is in now — has no successor and is open-ended.
            let to = self.history.get(index + 1).map_or(
                time::OffsetDateTime::new_utc(time::Date::MAX, time::Time::MIDNIGHT),
                |next| next.at,
            );
            if to > change.at {
                idle.push((change.at, to));
            }
        }
        idle
    }

    /// Split a direction's energy across the quarter hours it touched, cut also
    /// wherever the session changed state, with the energy of every gap between
    /// two readings spread over the time the session was **charging** in it.
    ///
    /// The input `mako-emob` needs to assign each quarter hour to the balance
    /// group of the supplier the driver chose `[A6 §IV.1]` —
    /// [`SessionSplit::market_series`] sums the slices of a quarter hour back
    /// together for exactly that.
    ///
    /// # Why the state changes cut it
    ///
    /// A quarter hour is where the energy settles. It is not where the price
    /// changes: `[AFIR Art. 5(4)]` prices time connected and **not** charging
    /// separately from the energy, and a vehicle stops charging when it stops
    /// charging rather than at `:15:00`. Split on the grid alone, the quarter
    /// hour a charge finishes in carries one `charging` flag for fifteen
    /// minutes that were not all the same — so up to a quarter hour of
    /// occupancy is billed as charging, or the reverse, at every transition.
    /// The session's own history says where the line falls, so it is drawn
    /// there.
    ///
    /// # …and why the state changes shape the interpolation too
    ///
    /// The same history says where the register *could* have moved. A
    /// transaction that opens `EVConnected`, starts charging thirty seconds
    /// later and sends its next meter value at the quarter hour — the ordinary
    /// OCPP 2.0.1 shape — has a register that did not move for those thirty
    /// seconds, and a straight line from the opening reading attributes energy
    /// to an interval the operator's own record calls suspended. So the split
    /// is handed [`Self::idle_intervals`] and holds the register flat across
    /// them; see [`crate::split::into_periods`].
    ///
    /// # Errors
    ///
    /// [`SessionError::NoSeries`] when the direction has no readings, or
    /// [`SessionError::Split`] when the readings cannot be split.
    pub fn split(&self, direction: Direction) -> Result<SessionSplit, SessionError> {
        let series = self
            .series_for(direction)
            .ok_or(SessionError::NoSeries { direction })?;
        let cuts: Vec<time::OffsetDateTime> = self.history.iter().map(|change| change.at).collect();
        into_periods(series, &cuts, &self.idle_intervals()).map_err(SessionError::Split)
    }
}

/// What can be wrong with a session.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// The protocol does not allow this transition.
    #[error("a session cannot go from {from:?} to {to:?}")]
    IllegalTransition {
        /// Where it was.
        from: SessionState,
        /// Where it was asked to go.
        to: SessionState,
    },

    /// Something was added after the session ended.
    #[error("the session has ended; nothing further may be added to it")]
    AlreadyEnded,

    /// The end precedes the start.
    #[error("the session ends at {ended_at} but starts at {started_at}")]
    EndsBeforeItStarts {
        /// When it started.
        started_at: time::OffsetDateTime,
        /// When it supposedly ended.
        ended_at: time::OffsetDateTime,
    },

    /// A transition is dated before the one it follows.
    #[error("a transition at {at} follows one at {previous}: the history must be ordered")]
    TimeWentBackwards {
        /// The last transition's instant.
        previous: time::OffsetDateTime,
        /// The instant offered.
        at: time::OffsetDateTime,
    },

    /// A second series for a direction that already has one.
    #[error("the session already has a {direction} series")]
    DuplicateDirection {
        /// Which direction.
        direction: Direction,
    },

    /// The direction has no readings.
    #[error("the session has no {direction} readings")]
    NoSeries {
        /// Which direction.
        direction: Direction,
    },

    /// The readings could not be split across quarter hours.
    #[error(transparent)]
    Split(#[from] SplitError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Authorization;
    use crate::meter::{MeterReading, ReadingContext};
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use time::macros::datetime;

    fn kwh(s: &str) -> Energy {
        Energy::from_kwh(Decimal::from_str(s).unwrap()).unwrap()
    }

    fn at(minute: i64) -> time::OffsetDateTime {
        datetime!(2026-01-02 10:00 +1) + time::Duration::minutes(minute)
    }

    fn session() -> Session {
        Session::open(
            "s-1".parse().unwrap(),
            "DE*AB7*E840*6487".parse().unwrap(),
            Authorization::ad_hoc(),
            at(0),
        )
    }

    fn import_series() -> MeterSeries {
        MeterSeries::new(
            Direction::Import,
            vec![
                MeterReading::new(
                    at(0),
                    kwh("100.000"),
                    Direction::Import,
                    ReadingContext::TransactionBegin,
                ),
                MeterReading::new(
                    at(30),
                    kwh("118.000"),
                    Direction::Import,
                    ReadingContext::TransactionEnd,
                ),
            ],
        )
        .unwrap()
    }

    #[test]
    fn a_session_runs_its_normal_course() {
        let mut s = session();
        assert_eq!(s.state(), SessionState::Pending);
        s.transition_to(SessionState::Charging, at(1)).unwrap();
        s.transition_to(SessionState::Suspended, at(20)).unwrap();
        s.transition_to(SessionState::Charging, at(25)).unwrap();
        s.attach_series(import_series()).unwrap();
        s.end(at(30), EndReason::Local).unwrap();

        assert_eq!(s.state(), SessionState::Ended);
        assert_eq!(s.end_reason, Some(EndReason::Local));
        assert_eq!(s.duration(), Some(time::Duration::minutes(30)));
        assert_eq!(
            s.total(Direction::Import).unwrap().to_string(),
            "18.000 kWh"
        );
        assert_eq!(
            s.history().len(),
            5,
            "pending, charging, suspended, charging, ended"
        );
    }

    #[test]
    fn the_history_answers_when_rather_than_only_what() {
        // "Suspended" with no timestamp cannot say for how long, and the
        // occupancy fee of AFIR Art. 5(4) is a price per minute of exactly
        // that interval.
        let mut s = session();
        s.transition_to(SessionState::Charging, at(0)).unwrap();
        s.transition_to(SessionState::Suspended, at(40)).unwrap();
        s.end(at(70), EndReason::Local).unwrap();

        assert_eq!(s.state_at(at(10)), Some(SessionState::Charging));
        assert_eq!(s.state_at(at(50)), Some(SessionState::Suspended));
        assert_eq!(s.state_at(at(-1)), None, "before it started");

        assert!(
            s.suspended_throughout(at(45), at(60)),
            "half an hour of occupancy, and the tariff may price it"
        );
        assert!(
            !s.suspended_throughout(at(30), at(50)),
            "charging for part of it"
        );
        assert!(
            !s.suspended_throughout(at(60), at(80)),
            "the session ended at 70"
        );
    }

    #[test]
    fn charging_throughout_is_not_the_negation_of_suspended_throughout() {
        // Four states, two of which are neither — and the gap between the two
        // questions is where an occupancy fee gets charged for time nobody
        // measured. `emob-cdr` fills the unmetered part of a session's window
        // from this, and it has to ask "did the record say charging" rather
        // than "did it fail to say suspended".
        let mut s = session();
        s.transition_to(SessionState::Charging, at(20)).unwrap();
        s.transition_to(SessionState::Suspended, at(40)).unwrap();
        s.end(at(70), EndReason::Local).unwrap();

        // Pending: neither. Connected, authorised, no energy moving — which is
        // exactly what the fee is for.
        assert!(!s.charging_throughout(at(0), at(20)));
        assert!(!s.suspended_throughout(at(0), at(20)));

        assert!(s.charging_throughout(at(20), at(40)));
        assert!(!s.suspended_throughout(at(20), at(40)));

        assert!(!s.charging_throughout(at(45), at(60)));
        assert!(s.suspended_throughout(at(45), at(60)));

        // Ended: neither, again, and the car may still be in the bay.
        assert!(!s.charging_throughout(at(75), at(90)));
        assert!(!s.suspended_throughout(at(75), at(90)));

        // A transition strictly inside disqualifies either answer: the interval
        // held two states, so neither is true *throughout* it.
        assert!(!s.charging_throughout(at(30), at(50)));
        assert!(!s.suspended_throughout(at(30), at(50)));

        // …and an empty or reversed interval is nothing at all.
        assert!(!s.charging_throughout(at(30), at(30)));
        assert!(!s.charging_throughout(at(30), at(20)));
    }

    #[test]
    fn a_history_that_runs_backwards_is_refused() {
        let mut s = session();
        s.transition_to(SessionState::Charging, at(20)).unwrap();
        assert!(matches!(
            s.transition_to(SessionState::Suspended, at(10)),
            Err(SessionError::TimeWentBackwards { .. })
        ));
        assert_eq!(s.state(), SessionState::Charging, "and nothing moved");
    }

    #[test]
    fn nothing_follows_the_end() {
        let mut s = session();
        s.transition_to(SessionState::Charging, at(0)).unwrap();
        s.end(at(30), EndReason::Local).unwrap();

        // Not a second end…
        assert!(matches!(
            s.end(at(40), EndReason::Remote),
            Err(SessionError::IllegalTransition { .. })
        ));
        // …not a resumption…
        assert!(matches!(
            s.transition_to(SessionState::Charging, at(40)),
            Err(SessionError::IllegalTransition { .. })
        ));
        // …and not a late series, which is a duplicate or a different session.
        assert!(matches!(
            s.attach_series(import_series()),
            Err(SessionError::AlreadyEnded)
        ));
    }

    #[test]
    fn a_session_cannot_go_back_to_pending() {
        let mut s = session();
        s.transition_to(SessionState::Charging, at(0)).unwrap();
        assert!(matches!(
            s.transition_to(SessionState::Pending, at(5)),
            Err(SessionError::IllegalTransition { .. })
        ));
    }

    #[test]
    fn a_session_cannot_end_before_it_starts() {
        let mut s = session();
        assert!(matches!(
            s.end(at(-5), EndReason::Local),
            Err(SessionError::EndsBeforeItStarts { .. })
        ));
        assert_eq!(
            s.state(),
            SessionState::Pending,
            "and the state is untouched"
        );
    }

    #[test]
    fn a_direction_gets_one_series() {
        let mut s = session();
        s.attach_series(import_series()).unwrap();
        assert!(matches!(
            s.attach_series(import_series()),
            Err(SessionError::DuplicateDirection { .. })
        ));
    }

    #[test]
    fn a_bidirectional_session_keeps_its_two_registers_apart() {
        let mut s = session();
        s.attach_series(import_series()).unwrap();
        s.attach_series(
            MeterSeries::new(
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
            .unwrap(),
        )
        .unwrap();

        assert_eq!(s.total(Direction::Import).unwrap(), kwh("18.000"));
        assert_eq!(s.total(Direction::Export).unwrap(), kwh("5"));
        // 18 and 5, never 13.
    }

    #[test]
    fn no_readings_is_not_zero() {
        let s = session();
        assert_eq!(
            s.total(Direction::Import),
            None,
            "a caller treating this as zero would invoice a session it has no readings for"
        );
    }

    #[test]
    fn the_idle_intervals_are_every_stretch_the_session_was_not_charging() {
        let mut s = session();
        s.transition_to(SessionState::Charging, at(1)).unwrap();
        s.transition_to(SessionState::Suspended, at(20)).unwrap();
        s.transition_to(SessionState::Charging, at(25)).unwrap();
        assert_eq!(
            s.idle_intervals(),
            vec![(at(0), at(1)), (at(20), at(25))],
            "pending at the start, then the pause"
        );
        s.end(at(30), EndReason::Local).unwrap();
        assert_eq!(
            s.idle_intervals().last().map(|(from, _)| *from),
            Some(at(30)),
            "…and the time after the end, which no reading can fall in"
        );

        // A session that ends without ever charging is idle from start to end.
        let mut never = session();
        never.end(at(10), EndReason::Local).unwrap();
        assert_eq!(never.idle_intervals()[0], (at(0), at(10)));
    }

    #[test]
    fn the_split_holds_the_register_flat_while_the_session_says_nothing_flowed() {
        // The ordinary OCPP 2.0.1 shape: `EVConnected` at the opening reading,
        // charging thirty seconds later, the next meter value at the quarter
        // hour. The thirty seconds carry no energy, whatever a straight line
        // between the readings would have said.
        let mut s = session();
        s.transition_to(SessionState::Suspended, at(0)).unwrap();
        let charging_from = at(0) + time::Duration::seconds(30);
        s.transition_to(SessionState::Charging, charging_from)
            .unwrap();
        s.attach_series(
            MeterSeries::new(
                Direction::Import,
                vec![
                    MeterReading::new(
                        at(0),
                        kwh("100.000"),
                        Direction::Import,
                        ReadingContext::TransactionBegin,
                    ),
                    MeterReading::new(
                        at(15),
                        kwh("105.000"),
                        Direction::Import,
                        ReadingContext::SampleClock,
                    ),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        let split = s.split(Direction::Import).unwrap();
        assert_eq!(split.slots[0].to, charging_from);
        assert!(split.slots[0].energy.is_zero());
        assert_eq!(split.slots[1].energy, kwh("5.000"));
        assert!(split.conserves());
    }

    #[test]
    fn a_session_splits_into_quarter_hours() {
        let mut s = session();
        s.attach_series(import_series()).unwrap();
        let split = s.split(Direction::Import).unwrap();
        assert_eq!(split.slots.len(), 2);
        assert!(split.conserves());

        assert!(matches!(
            s.split(Direction::Export),
            Err(SessionError::NoSeries { .. })
        ));
    }

    #[test]
    fn the_transition_table_is_exhaustive_and_final_is_final() {
        use SessionState::{Charging, Ended, Pending, Suspended};
        for next in [Pending, Charging, Suspended, Ended] {
            assert!(!Ended.can_transition_to(next), "nothing follows Ended");
        }
        assert!(Pending.can_transition_to(Charging));
        assert!(Pending.can_transition_to(Ended));
        assert!(!Charging.can_transition_to(Pending));
        assert!(Suspended.can_transition_to(Charging));
    }
}
