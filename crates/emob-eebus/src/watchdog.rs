//! The timer that decides which of `[LPC §2.3.2]`'s five states a site is in.
//!
//! # What a site draws while nobody is speaking to it
//!
//! `emob-smart` reads the five states as a **rule** and says why (D302): three
//! of them allow full power and are three different facts, and a platform whose
//! model has "limited" and "unlimited" cannot write the difference down, let
//! alone report it. A depot at full power because its grid operator allows it
//! and one at full power because the control box died four hours ago are the
//! same number and different problems.
//!
//! What that crate has no way to know is *which* of the five is true right now,
//! because deciding it means noticing a silence, and noticing a silence needs a
//! clock. This is the piece that does — and it does it the way everything else
//! here does: the instant is an argument, so a state at 21:00 is the same call
//! whether it is asked at 21:00 or two years later in a dispute.
//!
//! # Two timers, and only one of them is in the specification
//!
//! `[LPC-912]` puts a site into its failsafe when the Energy Guard has been
//! silent for the **Failsafe Duration Minimum** — a period the guard itself
//! writes, and which `[LPC-021]` gives a floor of two hours. That is the first
//! timer and it is a fact the wire carries.
//!
//! `[LPC-022]` is the second and it is the one every summary of § 14a drops: a
//! site leaves the failsafe again after between two and 24 hours, and runs on
//! its own connection. That is not a bug and it is not leniency — a building
//! throttled to 4.2 kW for a week because a control box died is not what
//! § 14a asks for, and a grid operator that believes it can still curtail such
//! a site is wrong about something it needs to be right about.
//!
//! So [`Watchdog::limit_at`] answers with three different states as a silence
//! lengthens, and each of them carries the instant the guard was last heard
//! from — because *"how long has this been true"* is the first question an
//! operator asks and the one a bare state cannot answer.

use rust_decimal::Decimal;

use emob_smart::GridLimit;

/// The floor `[LPC-021]` puts under the Failsafe Duration Minimum.
///
/// Two hours. A **default**, not a rule this crate applies on its own: the
/// actual period is written by the Energy Guard over
/// `deviceConfigurationKeyValue`, and until one arrives this is the shortest
/// period the specification admits — which is the reading that keeps a site
/// under control for longer rather than shorter.
pub const FAILSAFE_AFTER: time::Duration = time::Duration::hours(2);

/// Which of `[LPC §2.3.2]`'s five states a site is in, decided by a clock.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Watchdog {
    /// The failsafe value, in kW — what the site holds at while the Energy
    /// Guard is silent `[LPC-901/1]`.
    failsafe_kw: Decimal,
    /// How long a silence has to last before the failsafe applies `[LPC-912]`.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::duration_seconds"))]
    failsafe_after: time::Duration,
    /// How long the site stays in the failsafe before it runs on its own
    /// `[LPC-022]`.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::duration_seconds"))]
    autonomous_after: time::Duration,
    /// When the Energy Guard was last heard from. `None` until it ever has
    /// been, which is a site whose control box has never spoken.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339::option"))]
    heard_at: Option<time::OffsetDateTime>,
    /// The setpoint in force, in kW, and `None` where the guard is allowing
    /// full power.
    ///
    /// Two absences told apart by [`Self::heard_at`]: a guard that has released
    /// the site `[LPC-905]` and a guard that has never spoken.
    setpoint_kw: Option<Decimal>,
}

/// How long `[LPC-022]` lets a site sit in its failsafe before it runs on its
/// own connection.
///
/// The **shortest** of the range the requirement gives — two to 24 hours — for
/// the same reason [`FAILSAFE_AFTER`] takes the floor of its own: a site that
/// leaves the failsafe sooner is a site a grid operator stops believing it
/// controls sooner, and being wrong in that direction is the one that gets
/// noticed. An operator that has agreed a longer period says so.
const AUTONOMOUS_AFTER: time::Duration = time::Duration::hours(2);

impl Watchdog {
    /// A site whose control box has never spoken.
    ///
    /// `failsafe_kw` is what it will hold at once one has and then stops —
    /// `[LPC-901/1]`'s failsafe consumption active power limit, in kW, exactly.
    #[must_use]
    pub const fn new(failsafe_kw: Decimal, failsafe_after: time::Duration) -> Self {
        Self {
            failsafe_kw,
            failsafe_after,
            autonomous_after: AUTONOMOUS_AFTER,
            heard_at: None,
            setpoint_kw: None,
        }
    }

    /// …and how long it holds there before `[LPC-022]` releases it.
    #[must_use]
    pub const fn released_after(mut self, after: time::Duration) -> Self {
        self.autonomous_after = after;
        self
    }

    /// The Energy Guard was heard from — a heartbeat, a write, anything.
    ///
    /// `[LPC §3.8]`'s heartbeat is what this is normally fed by, and it is the
    /// only thing that keeps the failsafe at bay.
    pub const fn heard_from(&mut self, at: time::OffsetDateTime) {
        self.heard_at = Some(at);
    }

    /// A setpoint arrived and was accepted.
    ///
    /// Also counts as hearing from the guard: a write is a message, and a site
    /// that fell into its failsafe while being actively curtailed would be
    /// obeying a limit nobody had stopped sending.
    pub const fn limited_to(&mut self, kw: Decimal, at: time::OffsetDateTime) {
        self.setpoint_kw = Some(kw);
        self.heard_at = Some(at);
    }

    /// The guard released the site `[LPC-905]`.
    ///
    /// A **deactivation**, which the implementation guide §2.13 wants sent
    /// rather than inferred from silence — and which is why this is a method
    /// and not the absence of one.
    pub const fn released(&mut self, at: time::OffsetDateTime) {
        self.setpoint_kw = None;
        self.heard_at = Some(at);
    }

    /// When the Energy Guard was last heard from.
    #[must_use]
    pub const fn heard_at(&self) -> Option<time::OffsetDateTime> {
        self.heard_at
    }

    /// Which state the site is in, and what it may draw.
    ///
    /// The five of `[LPC §2.3.2]`, in the order a silence walks through them:
    ///
    /// | Silence | State | Ceiling |
    /// |---|---|---|
    /// | never spoken | `Uncontrolled` | the site's own connection |
    /// | speaking, curtailed | `limited` | the setpoint |
    /// | speaking, released | `unlimited/controlled` | the connection |
    /// | past the Failsafe Duration Minimum | `failsafe state` | the failsafe value |
    /// | past `[LPC-022]`'s release | `unlimited/autonomous` | the connection |
    ///
    /// `Uncontrolled` is not a state LPC has, and deliberately: LPC describes a
    /// commissioned system, and a site nobody has connected a control box to
    /// has no state machine to be in. It is a different fact from every one of
    /// the four, and `emob_smart::GridLimit` is where all five live because the
    /// difference between them is a *rule* rather than a transport (D302).
    #[must_use]
    pub fn limit_at(&self, now: time::OffsetDateTime) -> GridLimit {
        let Some(heard_at) = self.heard_at else {
            return GridLimit::Uncontrolled;
        };
        let silent_for = now - heard_at;

        if silent_for < self.failsafe_after {
            return match self.setpoint_kw {
                Some(kw) => GridLimit::curtailed(kw, heard_at),
                None => GridLimit::InContact { heard_at },
            };
        }
        if silent_for < self.failsafe_after + self.autonomous_after {
            return GridLimit::failsafe(self.failsafe_kw, heard_at);
        }
        // `[LPC-022]`: the site leaves the failsafe and runs on its own. The
        // part every summary of § 14a drops, and the one a grid operator most
        // needs to know, because it is the moment it stops being able to
        // curtail a site it believes it can.
        GridLimit::Autonomous {
            silent_since: heard_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn kw(text: &str) -> Decimal {
        Decimal::from_str_exact(text).expect("a decimal")
    }

    fn at(minutes: i64) -> time::OffsetDateTime {
        datetime!(2026-06-01 18:00 +2) + time::Duration::minutes(minutes)
    }

    fn watchdog() -> Watchdog {
        Watchdog::new(kw("4.2"), FAILSAFE_AFTER)
    }

    #[test]
    fn a_site_no_control_box_has_ever_spoken_to_is_not_in_a_failsafe() {
        // `Uncontrolled` and `failsafe state` are the same ceiling on a site
        // with no connection headroom and completely different situations: one
        // is a depot nobody has commissioned and the other is a control box to
        // go and replace.
        assert_eq!(watchdog().limit_at(at(0)), GridLimit::Uncontrolled);
    }

    #[test]
    fn the_silence_walks_through_three_states_and_each_says_when_it_started() {
        let mut watchdog = watchdog();
        watchdog.limited_to(kw("5"), at(0));

        // Speaking, and curtailed.
        assert_eq!(watchdog.limit_at(at(30)).lpc_state(), Some("limited"));
        assert_eq!(watchdog.limit_at(at(30)).stated_kw(), Some(kw("5")));

        // Past the Failsafe Duration Minimum `[LPC-912]`.
        let failsafe = watchdog.limit_at(at(150));
        assert_eq!(failsafe.lpc_state(), Some("failsafe state"));
        assert_eq!(failsafe.stated_kw(), Some(kw("4.2")));

        // …and past `[LPC-022]`'s release, running on its own connection.
        let autonomous = watchdog.limit_at(at(300));
        assert_eq!(autonomous.lpc_state(), Some("unlimited/autonomous"));
        assert_eq!(
            autonomous.stated_kw(),
            None,
            "nothing above the site's own connection"
        );
        assert!(
            matches!(autonomous, GridLimit::Autonomous { silent_since } if silent_since == at(0)),
            "and it says since when: {autonomous:?}"
        );
    }

    #[test]
    fn a_release_is_a_message_and_not_a_silence() {
        // `[LPC-905]` against `[LPC-912]`: a guard that says *"draw what you
        // like"* and a guard that has stopped talking are the same ceiling and
        // the opposite situation, and only one of them is a fault.
        let mut watchdog = watchdog();
        watchdog.released(at(0));
        assert_eq!(
            watchdog.limit_at(at(30)).lpc_state(),
            Some("unlimited/controlled")
        );
        assert_eq!(
            watchdog.limit_at(at(150)).lpc_state(),
            Some("failsafe state"),
            "…and the same site an hour later is not"
        );
    }

    #[test]
    fn a_heartbeat_keeps_the_failsafe_at_bay_and_keeps_the_setpoint() {
        let mut watchdog = watchdog();
        watchdog.limited_to(kw("5"), at(0));
        watchdog.heard_from(at(100));
        // The heartbeat alone does not release the site: the setpoint stands
        // until a write replaces it, which is what `[LPC-004]`'s duration and
        // §2.13's explicit deactivation are both about.
        let still = watchdog.limit_at(at(150));
        assert_eq!(still.lpc_state(), Some("limited"));
        assert_eq!(still.stated_kw(), Some(kw("5")));
    }

    #[test]
    fn the_release_window_is_the_operators_and_the_constant_is_the_floor() {
        // `[LPC-022]` gives a range of two to 24 hours. The default takes the
        // shortest, because a site that leaves the failsafe sooner is one a
        // grid operator stops believing it controls sooner — and being wrong in
        // that direction is the one that gets noticed.
        let mut long = watchdog().released_after(time::Duration::hours(24));
        long.limited_to(kw("5"), at(0));
        assert_eq!(long.limit_at(at(300)).lpc_state(), Some("failsafe state"));
        assert_eq!(
            long.limit_at(at(60 * 27)).lpc_state(),
            Some("unlimited/autonomous")
        );
    }
}
