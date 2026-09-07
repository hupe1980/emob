//! `sited` — the site controller: the § 14a wire, both ends of it.
//!
//! # What it decides, and what it must not
//!
//! A grid operator writes a setpoint to a site over EEBUS. The site shares that
//! power out among its charge points and tells each one what it may draw over
//! OCPP. Three crates already own every rule in that sentence:
//!
//! | Crate | Owns |
//! |---|---|
//! | `emob-eebus` | the value, off the `ScaledNumber`, exactly — and which of `[LPC §2.3.2]`'s five states a silence has put the site in |
//! | `emob-smart` | what the site may draw, and what it keeps a claim to `[BK6-22-300 Ziff. 4.5]` |
//! | `emob-ocpp` | the charging profile a station obeys, at a purpose an optimiser cannot outrank `[BK6-22-300 Ziff. 4.6]` |
//!
//! What is here is the half none of them can have: **when**. A setpoint is a
//! message and a failsafe is a silence, and telling those apart needs a clock —
//! which is the one thing a domain crate in this workspace may not have.
//!
//! # The edge is where the WAN is not
//!
//! This is the only service in the workspace designed to run *at a site* rather
//! than in a data centre, and the reason is in the statute. `[LPC-912]` puts a
//! site into its failsafe when the Energy Guard goes quiet, and `[LPC-021]`
//! gives that timer a floor of two hours — so a site whose controller is on the
//! far side of a broken WAN link is a site that will be held at 4.2 kW because
//! *this* process could not be reached, not because a grid operator asked. The
//! decision belongs next to the connection point.
//!
//! # Nothing here is a rule
//!
//! [`Sited::on_limit`] reads a write through `emob_eebus::read_limit_write` and
//! does not look at the number. [`Sited::allocate`] calls
//! `emob_smart::allocate` and does not choose a ceiling.
//! [`Sited::charging_profiles`] calls `emob_ocpp::to_charging_profiles` and does
//! not choose a profile purpose. A daemon is the worst place to keep a rule,
//! because CI does not run it — and what this one keeps instead is the timer,
//! the record of what was asked for and what was answered, and the account of
//! what the crossing to a station could not carry.
//!
//! # The record § 14a actually asks for
//!
//! `[LPC §4.1.5]` makes the **pair** the operator's evidence: what the grid
//! operator asked for, and what was answered. Neither half alone is worth
//! anything — a setpoint nobody can show was honoured is a setpoint nobody
//! honoured, and an allocation with no instruction behind it is a site
//! throttling itself. [`Sited::log`] is that pair, in order, and it is the one
//! piece of state this service holds that is not a timer.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]

use emob_core::Crossing;
use emob_eebus::{EebusError, LimitWrite, Watchdog};
use emob_ocpp::Setpoint;
use emob_smart::{Allocation, Demand, GridLimit, Site};
use rust_decimal::Decimal;

/// Watts in a kilowatt. The wire speaks watts and `[BK6-22-300]` speaks
/// kilowatts, and the conversion is a shift.
const W_PER_KW: Decimal = Decimal::from_parts(1000, 0, 0, false, 0);

/// One entry of the record `[LPC §4.1.5]` asks an operator to be able to
/// produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    /// When it arrived.
    pub at: time::OffsetDateTime,
    /// What the Energy Guard asked for, exactly as the wire carried it.
    pub asked: LimitWrite,
    /// What this side answered.
    pub answered: Answer,
}

/// What a site answered a grid operator's write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// The limit was taken on, and the site is held at it.
    Accepted {
        /// The ceiling in force after it, in kW.
        ceiling_kw: Decimal,
    },
    /// The write could not be read, and a NACK went back.
    ///
    /// Never a substituted value: see `emob_eebus::EebusError`. A limit nobody
    /// sent, applied to a site that will obey it, is worse than a refusal the
    /// Energy Guard can act on.
    Refused {
        /// Why, in one sentence.
        reason: String,
    },
}

/// The site controller.
pub struct Sited {
    /// The site: which points are steuerbare Verbrauchseinrichtungen, and what
    /// the connection carries. `emob-smart`'s, and not this crate's to judge.
    pub site: Site,
    /// Which of `[LPC §2.3.2]`'s five states the site is in, by the clock.
    watchdog: Watchdog,
    /// What was asked for and what was answered, in order.
    log: Vec<Instruction>,
}

impl Sited {
    /// A site whose control box has never spoken.
    ///
    /// `failsafe_kw` is `[LPC-901/1]`'s failsafe consumption active power limit
    /// — what the site holds at once an Energy Guard has spoken and then stops
    /// — and `failsafe_after` is `[LPC-912]`'s Failsafe Duration Minimum, which
    /// the guard writes and `[LPC-021]` floors at two hours.
    #[must_use]
    pub fn new(site: Site, failsafe_kw: Decimal, failsafe_after: time::Duration) -> Self {
        Self {
            site,
            watchdog: Watchdog::new(failsafe_kw, failsafe_after),
            log: Vec::new(),
        }
    }

    /// A heartbeat from the Energy Guard `[LPC §3.8]`.
    ///
    /// The only thing that keeps the failsafe at bay, and the reason this
    /// service has a clock at all.
    pub const fn on_heartbeat(&mut self, at: time::OffsetDateTime) {
        self.watchdog.heard_from(at);
    }

    /// A limit write arrived.
    ///
    /// `resolved` is `eebus::spine::WriteRequest::resolved` — what the function
    /// *becomes* if the write is accepted. Reading `data` instead would take a
    /// partial write's omitted `isLimitActive` for `false` and turn a
    /// curtailment into a release back to full power (SPINE IG §3.3).
    ///
    /// # Errors
    ///
    /// Whatever `emob_eebus::read_limit_write` refuses — and the refusal is
    /// recorded before it is returned, because `[LPC §4.1.5]` makes a write
    /// that was *not* honoured as much a part of the record as one that was.
    pub fn on_limit(
        &mut self,
        resolved: &eebus::model::CmdData,
        at: time::OffsetDateTime,
    ) -> Result<GridLimit, EebusError> {
        let asked =
            match emob_eebus::read_limit_write(resolved, eebus::usecases::limitation::LIMIT_ID) {
                Ok(asked) => asked,
                Err(refusal) => {
                    self.log.push(Instruction {
                        at,
                        // The refusal has no readable write behind it, so what is
                        // recorded is the refusal itself — which is the half that
                        // matters when a grid operator asks why nothing changed.
                        asked: LimitWrite {
                            is_active: false,
                            watts: Decimal::ZERO,
                            valid_for: None,
                        },
                        answered: Answer::Refused {
                            reason: refusal.to_string(),
                        },
                    });
                    return Err(refusal);
                }
            };

        // A write that is not active is a **release** `[LPC-905]`, and the
        // implementation guide §2.13 wants one sent rather than inferred from
        // silence — which is why it is a different call and not an absence.
        if asked.is_active {
            self.watchdog.limited_to(asked.watts / W_PER_KW, at);
        } else {
            self.watchdog.released(at);
        }

        let limit = self.watchdog.limit_at(at);
        self.log.push(Instruction {
            at,
            asked,
            answered: Answer::Accepted {
                ceiling_kw: self.ceiling_kw(&limit),
            },
        });
        Ok(limit)
    }

    /// Which state the site is in now, and what it may draw.
    #[must_use]
    pub fn limit_at(&self, now: time::OffsetDateTime) -> GridLimit {
        self.watchdog.limit_at(now)
    }

    /// Share the site's power out under whatever is in force now.
    ///
    /// `demand` is the market plan, the departure schedule, the driver — every
    /// input that could compete with the grid operator's instruction, entering
    /// where `[BK6-22-300 Ziff. 4.6]`'s priority rule makes it structural
    /// rather than remembered.
    #[must_use]
    pub fn allocate(&self, demand: &[Demand], now: time::OffsetDateTime) -> Allocation {
        emob_smart::allocate(&self.site, demand, &self.watchdog.limit_at(now))
    }

    /// …and what each station is told, with the account of what the wire could
    /// not carry.
    #[must_use]
    pub fn charging_profiles(
        &self,
        demand: &[Demand],
        now: time::OffsetDateTime,
        profile_id: i32,
    ) -> Crossing<Vec<Setpoint>> {
        emob_ocpp::to_charging_profiles(&self.allocate(demand, now), profile_id)
    }

    /// What was asked for and what was answered, in order — `[LPC §4.1.5]`.
    #[must_use]
    pub fn log(&self) -> &[Instruction] {
        &self.log
    }

    /// The ceiling a state comes to for this site, in kW.
    ///
    /// Through `emob_smart::allocate` rather than by reading `stated_kw`
    /// directly: the setpoint is raised to the Mindestleistung where it falls
    /// below it `[BK6-22-300 Ziff. 4.5]` and capped at the connection, and a
    /// record that stated the raw figure would say the site was held somewhere
    /// it was not.
    fn ceiling_kw(&self, limit: &GridLimit) -> Decimal {
        emob_smart::allocate(&self.site, &[], limit).ceiling_kw()
    }
}
