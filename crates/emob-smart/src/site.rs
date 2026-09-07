//! A site, its devices, and its power shared out under a ceiling nothing here
//! can raise.
//!
//! # What "unoverridable" has to mean to be worth claiming
//!
//! `[BK6-22-300 Ziff. 4.6]` is explicit about the conflict this module exists
//! for:
//!
//! > Der Betreiber hat technisch sicherzustellen, dass im Fall konkurrierender
//! > Anforderungen mit anderweitigen Steuerungsmaßnahmen, insbesondere
//! > **marktlicher Laststeuerung**, der Reduzierung nach dieser Festlegung stets
//! > insoweit Vorrang eingeräumt wird, als die Anforderung des Netzbetreibers
//! > über die konkurrierende Anforderung hinausgeht oder dieser widerspricht.
//!
//! A dynamic tariff, a fleet's departure schedule and a driver in a hurry are
//! all *"anderweitige Steuerungsmaßnahmen"*, and every one of them is a reason
//! somebody writes code that reaches past the grid limit. So the limit is not a
//! check this module performs — it is the **only door an allocation exists
//! through**:
//!
//! - [`Allocation`]'s fields are private and it has no constructor;
//! - [`allocate`] is the one function that returns one, and it takes the
//!   [`GridLimit`];
//! - a market plan is a [`Demand`], which is an **input**.
//!
//! There is no arrangement of these types in which a caller states what it wants
//! and gets it. That is the difference between a rule and a habit, and it is the
//! same construction `Evidence::billable_energy` uses one crate along.
//!
//! # …and the one thing that is not an instruction at all
//!
//! Three of `[LPC §2.3.2]`'s five states allow full power, and one of them is a
//! grid operator that stopped answering. [`GridLimit`] keeps them apart because
//! nothing downstream can: a site drawing everything because it may and a site
//! drawing everything because its control box died are the same number, and only
//! one of them is somebody's problem. See that type for the whole of it.
//!
//! # A point is at zero or at its floor, never between
//!
//! `DIN EN 61851-1` signals the current over the pilot's duty cycle and cannot
//! express less than 6 A, so *"draw 1 kW"* is not an instruction a three-phase
//! point can carry out — it is a session that stops. A proportional share is
//! therefore the wrong arithmetic: dividing 10 kW across four boxes gives each
//! of them a number none of them can signal, and the site draws nothing while
//! reporting that it allocated everything.
//!
//! So the allocation seats points **in the order the caller hands them**, each
//! at its own floor first, and shares what is left over the seated ones. A point
//! that cannot be seated is held at zero and said so. `[BK6-22-300 Ziff. 4.5.2]`
//! is what makes the order the caller's: *"Der Betreiber ist berechtigt, den
//! insgesamt gewährten Sollwert … nach eigener Maßgabe einzusetzen."*

use emob_core::{APPORTIONED_SCALE, EvseId};
use rust_decimal::{Decimal, RoundingStrategy};

use crate::steuve::{Control, Minimum, SteuVe, minimum_for};

/// What a grid operator has told a site to draw, and when `[BK6-22-300
/// Ziff. 4.1]`.
///
/// The instant is carried because a curtailment is a thing that happened rather
/// than a state: an operator answering *"why did this site draw 7 kW at
/// eleven"* is answering with the setpoint that was in force then, and a
/// setpoint with no time on it cannot be replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Curtailment {
    /// The maximum netzwirksamer Leistungsbezug, in kW.
    pub setpoint_kw: Decimal,
    /// When the grid operator issued it.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub at: time::OffsetDateTime,
}

/// A site: what it is connected at, and the devices § 14a reaches.
///
/// The points that are **not** steuerbare Verbrauchseinrichtungen are absent by
/// construction — [`SteuVe::of`] refused them — which is what stops a public
/// fast charger being curtailed under a Festlegung that excludes it by name.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Site {
    /// What the whole site is connected at, in kW. A physical bound, and the
    /// one that holds when no grid operator has said anything.
    pub connection_kw: Decimal,
    /// The controllable devices behind it, in the order the operator wants them
    /// served.
    pub devices: Vec<SteuVe>,
}

impl Site {
    /// The Mindestleistung this site is owed `[BK6-22-300 Ziff. 4.5]`.
    #[must_use]
    pub fn minimum(&self) -> Minimum {
        minimum_for(&self.devices)
    }
}

/// The fallback that applies while the grid operator's controller is not
/// speaking `[LPC-021]`.
///
/// A value the Energy Guard writes onto the site rather than one the site
/// picks, and the reason a broken control box does not release a building onto
/// its full connection the moment it stops sending heartbeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Failsafe {
    /// `failsafeConsumptionActivePowerLimit` `[LPC-021]`, in kW.
    pub limit_kw: Decimal,
    /// When the Energy Guard was last heard from.
    ///
    /// Carried for the same reason a curtailment carries its instant: an
    /// operator answering *"why was this site at 4.2 kW all afternoon"* is
    /// answering with the moment the control box went quiet.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub silent_since: time::OffsetDateTime,
}

/// The ceiling in force, and which of the five states it came out of.
///
/// # Why this is five values and not an `Option`
///
/// § 14a is carried over EEBUS's *Limitation of Power Consumption* use case,
/// and `[LPC §2.3.2]` gives a controllable system five states rather than
/// two. Three of them allow full power and they are **not the same fact**:
///
/// - `unlimited/controlled` — the grid operator is there and is allowing it;
/// - `unlimited/autonomous` — the grid operator has been gone for hours
///   `[LPC-906]`, and the site is running on its own;
/// - and a site with no Energy Guard commissioned at all, where there is
///   nothing to fall silent.
///
/// A site drawing 60 kW because nobody is limiting it and a site drawing 60 kW
/// because its control box died look identical to every downstream reader
/// unless the type keeps them apart, and only one of them is somebody's problem
/// at two in the morning. So every way of saying *"nothing is limiting this
/// site"* has to say which of the three it means, and two of them put a note on
/// the allocation.
///
/// # What is not here
///
/// **The timers.** `[LPC-912]` gives 120 seconds without a heartbeat before the
/// failsafe applies and `[LPC-022]` holds it there for between two and 24 hours
/// before `unlimited/autonomous`. Those are the protocol engine's, beside the
/// socket that would notice the silence — the `eebus` crate runs that state
/// machine against a virtual clock and this one reads no clock at all. What
/// arrives here is the state it reached and the ceiling that came with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum GridLimit {
    /// No Energy Guard is commissioned for this site.
    ///
    /// Not a fault and not a fallback: a site whose points are steuerbare
    /// Verbrauchseinrichtungen but which no grid operator has yet connected a
    /// control box to. Its own connection is the only bound.
    Uncontrolled,

    /// `limited`: a setpoint from the grid operator is in force.
    Curtailed(Curtailment),

    /// `unlimited/controlled`: the Energy Guard is speaking and is allowing
    /// full power `[LPC-905]`.
    InContact {
        /// When it was last heard from.
        #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
        heard_at: time::OffsetDateTime,
    },

    /// `init` or `failsafe state`: the Energy Guard is not speaking, and the
    /// failsafe value applies `[LPC-901/1]`, `[LPC-912]`.
    UnderFailsafe(Failsafe),

    /// `unlimited/autonomous`: the Energy Guard has been absent longer than the
    /// Failsafe Duration Minimum, and the site runs on its own `[LPC-906]`.
    ///
    /// The failsafe is a floor under a communications fault, not a punishment
    /// for one: `[LPC-022]` holds a site there for between two and 24 hours and
    /// then releases it, because a building throttled to 4.2 kW for a week
    /// because a control box died is not what § 14a asks for.
    Autonomous {
        /// When the Energy Guard was last heard from.
        #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
        silent_since: time::OffsetDateTime,
    },
}

impl GridLimit {
    /// A setpoint from the grid operator.
    #[must_use]
    pub const fn curtailed(setpoint_kw: Decimal, at: time::OffsetDateTime) -> Self {
        Self::Curtailed(Curtailment { setpoint_kw, at })
    }

    /// The failsafe value, because the Energy Guard has gone quiet.
    #[must_use]
    pub const fn failsafe(limit_kw: Decimal, silent_since: time::OffsetDateTime) -> Self {
        Self::UnderFailsafe(Failsafe {
            limit_kw,
            silent_since,
        })
    }

    /// The ceiling this state states, in kW, or [`None`] where it states none.
    ///
    /// [`None`] is *"nothing above the site's own connection"*, which is what
    /// all three of the unlimited states come to — they differ in what an
    /// operator is owed an explanation about, not in the arithmetic.
    #[must_use]
    pub const fn stated_kw(&self) -> Option<Decimal> {
        match self {
            Self::Uncontrolled | Self::InContact { .. } | Self::Autonomous { .. } => None,
            Self::Curtailed(curtailment) => Some(curtailment.setpoint_kw),
            Self::UnderFailsafe(failsafe) => Some(failsafe.limit_kw),
        }
    }

    /// The state's name as `[LPC §2.3.2]` spells it.
    ///
    /// The specification's spelling rather than this crate's, so an operator
    /// comparing a site against a conformance log is comparing like with like.
    /// `Uncontrolled` has no LPC name because LPC has no state for a site with
    /// no Energy Guard: there is nothing to run the state machine.
    #[must_use]
    pub const fn lpc_state(&self) -> Option<&'static str> {
        match self {
            Self::Uncontrolled => None,
            Self::Curtailed(_) => Some("limited"),
            Self::InContact { .. } => Some("unlimited/controlled"),
            Self::UnderFailsafe(_) => Some("failsafe state"),
            Self::Autonomous { .. } => Some("unlimited/autonomous"),
        }
    }
}

/// What something wants — a market plan, a departure schedule, a driver.
///
/// An **input**. Everything that could compete with the grid operator's
/// instruction enters here, which is what makes `[BK6-22-300 Ziff. 4.6]`'s
/// priority rule structural rather than remembered.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Demand {
    /// Which point.
    pub evse_id: EvseId,
    /// What it is asking for, in kW.
    pub wanted_kw: Decimal,
}

/// Something the allocation had to say about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum SmartNote {
    /// The grid operator asked for less than the site is owed `[BK6-22-300
    /// Ziff. 4.5]`.
    ///
    /// The Festlegung gives the operator an *Anspruch* — *"hat der Betreiber …
    /// weiterhin einen Anspruch auf einen mindestens zu gewährenden
    /// netzwirksamen Leistungsbezug"* — so a setpoint below it is an instruction
    /// the grid operator may not give. The site holds at the minimum rather than
    /// obeying, and says so: obeying silently is giving up a legal claim, and
    /// nobody would ever find out that it had been given up.
    SetpointBelowMinimum {
        /// What the grid operator asked for.
        setpoint_kw: Decimal,
        /// What the site is owed.
        minimum_kw: Decimal,
    },

    /// A demand named a point this site does not control.
    ///
    /// The third answer beside [`Self::HeldAtZero`] and [`Self::Curtailed`],
    /// and the only one that is about the *site* rather than about a box. A
    /// demand reaching here for an EVSE that is not one of this site's
    /// steuerbare Verbrauchseinrichtungen means the energy-management system
    /// and the site controller disagree about what is behind the connection —
    /// and the disagreement is one-directional in the dangerous sense: the
    /// point draws whatever it draws, [`Allocation::within_ceiling`] cannot see
    /// it, and the ceiling `[BK6-22-300 Ziff. 4.6]` makes binding is being
    /// judged against a smaller site than the one that exists.
    ///
    /// Nothing here can fix that — a point this crate has no [`SteuVe`] for is
    /// a point it has no floor, no connection rating and no `Control` for, and
    /// seating it on a guess would put a number under the ceiling that no rule
    /// produced. So it is named, which is the whole of what the rule allows
    /// (D310).
    ///
    /// [`SteuVe`]: crate::SteuVe
    NotOnThisSite {
        /// Which point was asked for.
        evse_id: String,
    },

    /// A point could not be seated and is held at zero.
    ///
    /// Not a fault. `DIN EN 61851-1` cannot signal less than 6 A, so a point
    /// that cannot be given its own floor is a point that cannot charge — and
    /// naming it is what lets an operator tell *"the site is full"* from *"this
    /// box is broken"*.
    HeldAtZero {
        /// Which point.
        evse_id: String,
        /// The lowest power it could have carried out.
        floor_kw: Decimal,
    },

    /// A point was given less than it asked for.
    Curtailed {
        /// Which point.
        evse_id: String,
        /// What it asked for.
        wanted_kw: Decimal,
        /// What it got.
        allocated_kw: Decimal,
    },

    /// The grid operator's controller is not speaking, and the site is held at
    /// the failsafe value `[LPC-021]`.
    ///
    /// The one an operator most needs told. Nothing is wrong with the site: a
    /// depot at 4.2 kW under a failsafe looks exactly like a depot at 4.2 kW
    /// under a setpoint, and only one of the two is a control box to go and
    /// replace.
    UnderFailsafe {
        /// What the failsafe allows, in kW.
        limit_kw: Decimal,
        /// When the Energy Guard was last heard from.
        silent_since: time::OffsetDateTime,
    },

    /// The controller has been absent past the Failsafe Duration Minimum, and
    /// the site is running unlimited on its own `[LPC-906]`.
    ///
    /// The dangerous quiet. `[LPC-022]` says a site leaves the failsafe after
    /// between two and 24 hours, so this is the *correct* behaviour — and it
    /// means a site that a grid operator believes it can curtail is one that
    /// will not answer. Silent here would be the worst of the four notes to
    /// leave out.
    RunningAutonomously {
        /// When the Energy Guard was last heard from.
        silent_since: time::OffsetDateTime,
    },

    /// The site draws less than the grid operator allowed, because its own
    /// devices cannot use the rest.
    ///
    /// Worth saying: a grid operator reading a site at 7 kW under a 20 kW
    /// setpoint is reading a site that is not being curtailed at all, and the
    /// difference between *"we held you down"* and *"nobody was plugged in"* is
    /// the difference between a network problem and a Tuesday.
    Headroom {
        /// How much of the ceiling went unused, in kW.
        kw: Decimal,
    },
}

impl core::fmt::Display for SmartNote {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::SetpointBelowMinimum {
                setpoint_kw,
                minimum_kw,
            } => write!(
                f,
                "the grid operator asked for {setpoint_kw} kW and this site is owed {minimum_kw} kW [BK6-22-300 Ziff. 4.5]: it is held at the minimum, because obeying a setpoint below the Anspruch is giving up a claim nobody would ever find out had been given up"
            ),
            Self::NotOnThisSite { evse_id } => write!(
                f,
                "{evse_id} was asked for and is not a steuerbare Verbrauchseinrichtung of this site: the energy-management system and the site controller disagree about what is behind the connection, and a point this allocation cannot see is one the [BK6-22-300 Ziff. 4.6] ceiling is not being judged against"
            ),
            Self::HeldAtZero { evse_id, floor_kw } => write!(
                f,
                "{evse_id} is held at zero: the budget left cannot carry its {floor_kw} kW floor, and DIN EN 61851-1 has no duty cycle below 6 A that means charge slowly [BK6-22-300 Ziff. 4.6]"
            ),
            Self::Curtailed {
                evse_id,
                wanted_kw,
                allocated_kw,
            } => write!(
                f,
                "{evse_id} asked for {wanted_kw} kW and was given {allocated_kw} kW"
            ),
            Self::UnderFailsafe {
                limit_kw,
                silent_since,
            } => write!(
                f,
                "the Energy Guard has not been heard from since {silent_since} and this site is held at its failsafe value of {limit_kw} kW [LPC-021]: the ceiling is a communications fault rather than a grid operator's instruction, and the box that stopped talking is still not talking"
            ),
            Self::RunningAutonomously { silent_since } => write!(
                f,
                "the Energy Guard has not been heard from since {silent_since}, longer than the Failsafe Duration Minimum, so this site has left the failsafe and runs on its own connection [LPC-906]: it is drawing what it likes and the grid operator cannot currently reduce it"
            ),
            Self::Headroom { kw } => write!(
                f,
                "{kw} kW of the ceiling went unused: this site is not being held down by it"
            ),
        }
    }
}

/// What every point may draw, under a ceiling this type cannot be built outside
/// of.
///
/// No constructor and no public fields, on purpose: see the module
/// documentation. [`allocate`] is the only thing that makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Allocation {
    seats: Vec<(EvseId, Decimal)>,
    ceiling_kw: Decimal,
    notes: Vec<SmartNote>,
}

impl Allocation {
    /// The ceiling that was in force.
    #[must_use]
    pub const fn ceiling_kw(&self) -> Decimal {
        self.ceiling_kw
    }

    /// What one point may draw, in kW. Zero for a point that could not be
    /// seated.
    #[must_use]
    pub fn for_point(&self, evse_id: &EvseId) -> Decimal {
        self.seats
            .iter()
            .find(|(id, _)| id == evse_id)
            .map_or(Decimal::ZERO, |(_, kw)| *kw)
    }

    /// Every seated point and its share, in the order they were seated.
    #[must_use]
    pub fn seats(&self) -> &[(EvseId, Decimal)] {
        &self.seats
    }

    /// What the site will draw, in kW.
    #[must_use]
    pub fn total_kw(&self) -> Decimal {
        self.seats.iter().map(|(_, kw)| *kw).sum()
    }

    /// How much of the ceiling is unused.
    #[must_use]
    pub fn headroom_kw(&self) -> Decimal {
        self.ceiling_kw - self.total_kw()
    }

    /// Whether the allocation is inside its ceiling.
    ///
    /// **True by construction**, and re-checkable for the same reason
    /// `Cdr::conserves` is: an `Allocation` can arrive over a wire inside a
    /// site controller's state, and a claim that is cheap to re-ask is one a
    /// reviewer does not have to take on trust.
    #[must_use]
    pub fn within_ceiling(&self) -> bool {
        self.total_kw() <= self.ceiling_kw
    }

    /// Everything the allocation had to say.
    #[must_use]
    pub fn notes(&self) -> &[SmartNote] {
        &self.notes
    }

    /// One line per note, for an operator queue.
    pub fn reasons(&self) -> impl Iterator<Item = String> + '_ {
        self.notes.iter().map(ToString::to_string)
    }
}

/// The ceiling in force, and every note the state it came out of owes a reader.
///
/// Split out of [`allocate`] because it is a different question: that one shares
/// a budget between points, and this one decides what the budget *is* — out of
/// `[LPC §2.3.2]`'s five states, `[BK6-22-300 Ziff. 4.5]`'s Anspruch, and the
/// site's own connection, in that order.
fn ceiling_for(
    site: &Site,
    limit: &GridLimit,
    notes: &mut Vec<SmartNote>,
    minimum: Minimum,
) -> Decimal {
    // Two of the three states that state no ceiling say so for a reason
    // somebody has to hear about, which is why they are notes rather than an
    // absent `Option`.
    match limit {
        GridLimit::UnderFailsafe(failsafe) => notes.push(SmartNote::UnderFailsafe {
            limit_kw: failsafe.limit_kw,
            silent_since: failsafe.silent_since,
        }),
        GridLimit::Autonomous { silent_since } => notes.push(SmartNote::RunningAutonomously {
            silent_since: *silent_since,
        }),
        GridLimit::Uncontrolled | GridLimit::Curtailed(_) | GridLimit::InContact { .. } => {}
    }
    // The Anspruch holds against a failsafe exactly as it holds against a
    // setpoint. `[BK6-22-300 Ziff. 4.5]` guarantees the Mindestleistung is a
    // draw the grid can always carry, so falling back to a value below it is
    // not the cautious reading — it is giving up the claim for a reason the
    // Festlegung does not recognise, and doing it while nobody is watching.
    let allowed = limit.stated_kw().map_or(site.connection_kw, |stated_kw| {
        if stated_kw < minimum.kw && !site.devices.is_empty() {
            notes.push(SmartNote::SetpointBelowMinimum {
                setpoint_kw: stated_kw,
                minimum_kw: minimum.kw,
            });
            minimum.kw
        } else {
            stated_kw
        }
    });
    // …and never above what the site is physically connected at, which no
    // instruction can raise either.
    allowed.min(site.connection_kw)
}

/// Share a site's power out — the **one** door an [`Allocation`] comes through.
///
/// # The order of the two bounds
///
/// The ceiling is the smaller of what the site is physically connected at and
/// what the grid operator has allowed, and the grid operator's figure is first
/// raised to the Mindestleistung where it falls below it — because
/// `[BK6-22-300 Ziff. 4.5]` gives the operator a claim to that power, and a
/// site that quietly obeys a setpoint below it has given the claim up.
///
/// The demand is then seated against that, never the other way round. A market
/// plan asking for every point's full rated power under a 7 kW setpoint gets
/// 7 kW, which is `[BK6-22-300 Ziff. 4.6]`'s priority rule and not a policy this
/// crate chose.
///
/// ```
/// # use emob_smart::{Control, Demand, GridLimit, Site, SteuVe, allocate};
/// # use emob_core::station::{Accessibility, ChargePointProfile};
/// # use rust_decimal::Decimal;
/// # use time::macros::datetime;
/// # let mut depot = ChargePointProfile::bare("DE*ABC*E00001".parse()?, datetime!(2026-01-01 0:00 UTC).date());
/// # depot.accessibility = Accessibility::Private;
/// let box_one = SteuVe::of(&depot, Control::Ems)?;
/// let site = Site { connection_kw: Decimal::from(22), devices: vec![box_one.clone()] };
///
/// // A market plan wants everything the box can take.
/// let plan = [Demand { evse_id: box_one.evse_id().clone(), wanted_kw: Decimal::from(11) }];
///
/// // …and the grid operator has said 5 kW.
/// let limit = GridLimit::curtailed(Decimal::from(5), datetime!(2026-06-01 18:00 +2));
/// let allocation = allocate(&site, &plan, &limit);
///
/// assert!(allocation.total_kw() <= allocation.ceiling_kw());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn allocate(site: &Site, demand: &[Demand], limit: &GridLimit) -> Allocation {
    let mut notes = Vec::new();
    let minimum = site.minimum();

    // The grid operator's figure, raised to what the site is owed. `[BK6-22-300
    // Ziff. 4.5]` is an *Anspruch*, so a setpoint below it is an instruction the
    // operator may not give — and holding at the minimum is the only answer that
    // leaves the claim on the record.
    let ceiling_kw = ceiling_for(site, limit, &mut notes, minimum);

    // Seat each point at its own floor, in the order the operator handed them
    // — `[BK6-22-300 Ziff. 4.5.2]` makes that the operator's to decide. A point
    // whose floor does not fit is held at zero, because there is no duty cycle
    // between zero and 6 A.
    // Asked before anything is seated, because a demand for a point this site
    // has no `SteuVe` for is a fact about the site rather than about the
    // budget: it drops out of the loop below silently, and the ceiling is then
    // judged against a smaller site than the one that exists. See
    // `SmartNote::NotOnThisSite`.
    for want in demand {
        if !site
            .devices
            .iter()
            .any(|device| device.evse_id() == &want.evse_id)
        {
            notes.push(SmartNote::NotOnThisSite {
                evse_id: want.evse_id.to_string(),
            });
        }
    }

    let mut budget = ceiling_kw;
    let mut seats: Vec<(EvseId, Decimal)> = Vec::new();
    for device in &site.devices {
        let Some(wanted) = demand
            .iter()
            .find(|want| &want.evse_id == device.evse_id())
            .map(|want| want.wanted_kw)
        else {
            continue;
        };
        if wanted <= Decimal::ZERO {
            continue;
        }
        let floor = device.technical_floor_kw();
        if floor > budget {
            notes.push(SmartNote::HeldAtZero {
                evse_id: device.evse_id().to_string(),
                floor_kw: floor,
            });
            continue;
        }
        budget -= floor;
        seats.push((device.evse_id().clone(), floor));
    }

    // …and share what is left over the seated points, each up to what it asked
    // for and up to what it is connected at.
    //
    // **Water-filling, sequentially.** The obvious form — divide the budget by
    // the hungry count and hand every point that share — is wrong twice. It
    // over-spends, because `Decimal` division is not exact and `n` shares of
    // `budget / n` can come to `budget` plus one unit in the twenty-eighth
    // place: an ulp, and on the wrong side of a ceiling a grid operator set. And
    // it under-spends, because a point that hits its own limit leaves the rest
    // unclaimed until another pass.
    //
    // So each point in turn takes at most an equal share of what is **still**
    // left, and what it does not take stays for the ones behind it. Every take
    // is bounded by the remaining budget by construction, so the sum cannot
    // exceed it however the division rounded — the same argument the settlement
    // split makes about a telescoping sum, in a loop rather than a series.
    let headroom_for = |device: &SteuVe, seated: Decimal, wanted: Decimal| {
        wanted.min(device.connection_kw()) - seated
    };
    loop {
        let hungry: Vec<usize> = (0..seats.len())
            .filter(|index| hunger(site, demand, &seats[*index], headroom_for) > Decimal::ZERO)
            .collect();
        if hungry.is_empty() || budget <= Decimal::ZERO {
            break;
        }
        let mut left = hungry.len();
        let before = budget;
        for index in hungry {
            if budget <= Decimal::ZERO {
                break;
            }
            // Quoted at a scale a **sum** of these can carry, and rounded
            // toward zero. `budget / 7` spends the whole 96-bit mantissa on a
            // repeating fraction, and seven of those cannot be added exactly —
            // the total comes back one unit in the twenty-eighth place high,
            // which is a site over a ceiling a grid operator set. It is the same
            // failure `emob_core::apportion` documents and the same answer:
            // quote the share where a sum of them is exact. A picowatt is nine
            // orders finer than anything a pilot can signal.
            let share = (budget / Decimal::from(u64::try_from(left).unwrap_or(1)))
                .round_dp_with_strategy(APPORTIONED_SCALE, RoundingStrategy::ToZero)
                .min(budget);
            let take = share.min(hunger(site, demand, &seats[index], headroom_for));
            seats[index].1 += take;
            budget -= take;
            left = left.saturating_sub(1).max(1);
        }
        // A pass that moved nothing will not move anything on the next one
        // either, and a loop that cannot make progress is one that does not
        // terminate.
        if budget >= before {
            break;
        }
    }

    for (id, allocated) in &seats {
        if let Some(wanted) = demand
            .iter()
            .find(|want| &want.evse_id == id)
            .map(|want| want.wanted_kw)
            && *allocated < wanted
        {
            notes.push(SmartNote::Curtailed {
                evse_id: id.to_string(),
                wanted_kw: wanted,
                allocated_kw: *allocated,
            });
        }
    }
    let total: Decimal = seats.iter().map(|(_, kw)| *kw).sum();
    if total < ceiling_kw {
        notes.push(SmartNote::Headroom {
            kw: ceiling_kw - total,
        });
    }

    Allocation {
        seats,
        ceiling_kw,
        notes,
    }
}

/// How much more one seated point could take — bounded by what it asked for and
/// by what it is connected at.
fn hunger(
    site: &Site,
    demand: &[Demand],
    seat: &(EvseId, Decimal),
    headroom_for: impl Fn(&SteuVe, Decimal, Decimal) -> Decimal,
) -> Decimal {
    let (Some(device), Some(wanted)) = (
        site.devices
            .iter()
            .find(|device| device.evse_id() == &seat.0),
        demand
            .iter()
            .find(|want| want.evse_id == seat.0)
            .map(|want| want.wanted_kw),
    ) else {
        return Decimal::ZERO;
    };
    headroom_for(device, seat.1, wanted)
}

/// Whether a set of devices is controlled the way `[BK6-22-300 Ziff. 4.4.b]`
/// describes — one setpoint for the whole group.
///
/// The question a site controller asks before it accepts a single figure for
/// everything: a device on `4.4.a` gets its own setpoint from the grid operator
/// and is not the energy-management system's to spend.
#[must_use]
pub fn is_one_ems_group(devices: &[SteuVe]) -> bool {
    !devices.is_empty()
        && devices
            .iter()
            .all(|device| device.control() == Control::Ems)
}
