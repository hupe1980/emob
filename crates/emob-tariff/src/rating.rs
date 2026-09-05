//! Turning a session into money.
//!
//! # A session is a sequence of periods, and that is not a detail
//!
//! Rating the whole session against one tariff element is the reading almost
//! every implementation starts with, and it is wrong in a way that only shows
//! up on tiered tariffs. "The first 10 kWh at 0.39, the rest at 0.59" is a
//! restriction on *how much has been delivered so far* `[OCPI 2.3.0 §Tariff]`.
//! Judged against the session total instead, a 50 kWh session reprices all
//! 50 kWh at 0.59 — including the first ten, which the driver was quoted at
//! 0.39.
//!
//! So [`rate`] walks [`Chargeable::periods`] in order, carries the cumulative
//! energy and duration, and asks which element applies **at the start of each
//! period**. The quarter-hour slots the settlement layer already produces are
//! exactly the right periods, so the split that conserves energy is also the
//! input that prices it.
//!
//! # …and the period is cut at the threshold, not at the quarter hour
//!
//! Asking at the start of each period is not enough on its own. A period that
//! delivers 15 kWh under "the first 10 kWh at 0.39, the rest at 0.59" begins
//! in the first tier, so all fifteen would be priced at 0.39 — the same
//! retroactive-repricing bug as before, moved down a level and made to depend
//! on how finely the caller happened to slice the session.
//!
//! [`rate`] therefore **cuts every period at each threshold that falls inside
//! it** before pricing anything. The energy is divided exactly at the
//! threshold, because the energy is the quantity being tiered and the quantity
//! being settled; the time is divided in proportion, to the second. Ten of the
//! fifteen are charged at 0.39 and five at 0.59, and the answer no longer
//! depends on whether the session arrived as one period or ninety-six.
//!
//! ## …except for the one restriction that carries no information to cut on
//!
//! That holds for every quantity that **accumulates** — energy delivered so
//! far, seconds elapsed so far, the wall clock — and it holds for a reason
//! worth stating: a period *contains* the fact about where the threshold was
//! crossed. One period of 15 kWh says by construction that the tenth
//! kilowatt-hour fell inside it, so the cut can be placed and the answer stops
//! depending on the slicing.
//!
//! Average power is `energy / duration` over whatever window is asked about,
//! and a period carries **no** information about the power inside it. A session
//! delivering 60 kWh in an hour averages 60 kW; the same hour measured as two
//! half-hours of 55 and 5 kWh averages 110 kW and 10 kW. Under "below 50 kW at
//! 0.30, otherwise 0.60" those price differently, and no cut recovers the
//! second reading from the first — the 60 kW figure does not contain it.
//!
//! The finer answer is therefore not a different answer to the same question.
//! It is a **better measurement**, and the arithmetic is right either way; what
//! it cannot do is make a low-resolution input behave like a high-resolution
//! one. Rate the periods the meter produced — which is what
//! `emob_session::Session::split` hands over — and the answer is stable.
//! [`RatingNote::PowerJudgedPerPeriod`] says so on any tariff where it matters,
//! because a partner's document may carry coarser periods than the meter did.
//!
//! # What is charged, and what it is charged against
//!
//! | Dimension | Quantity | Unit |
//! |---|---|---|
//! | `Energy` | delivered energy | kWh |
//! | `Time` | time in periods that were charging | hours |
//! | `ParkingTime` | time in periods that were not | hours |
//! | `Flat` | the session itself, once | once |
//!
//! # The element is chosen **per dimension**, not per period
//!
//! `[OCPI 2.3.0 §Tariff]`: "the first Tariff Element with a Price Component
//! **for that dimension** in the list with matching Tariff Restrictions will be
//! used. Only one Price Component per dimension can be active at any point in
//! time, but multiple Price Components for different dimensions can be active
//! at once."
//!
//! The shape that follows is the one partners send, because the specification
//! recommends it: an unrestricted default per dimension after the restricted
//! ones. A tariff written that way — `{FLAT 0.50}` then `{ENERGY 0.49}` — has
//! two unrestricted elements, and an engine that stops at the first bills the
//! session fee and *nothing else*: the kilowatt-hours vanish, no element failed
//! to match, and no note is raised.
//!
//! So [`matching_component`] asks one dimension at a time, and
//! [`RatingNote::Unpriced`] is per dimension too — the specification answers a
//! dimension nothing matched with "there will be no costs for that Tariff
//! Dimension", and the quantity that went unpriced is what a dispute turns on.
//!
//! # Every number the total is made of is kept
//!
//! [`rate`] returns a [`Rated`] carrying one [`Line`] per *distinct price* that
//! applied — so a tiered session yields two energy lines at two prices, which
//! is what a tiered invoice has to show. The total is the sum of the lines plus
//! at most one [`Adjustment`], and nothing else: there is no term in the total
//! that is not one of those two things, so "why is this €14.46" is answerable
//! by reading the structure rather than by re-deriving it.
//!
//! # The wall clock is read in the tariff's own zone
//!
//! "0.30 from 22:00" is a statement about **local civil time at the charge
//! point** `[OCPI 2.3.0 §mod_tariffs_tariffrestrictions_class]`, and OCPI
//! carries the zone it is read in on the Location, where it is mandatory
//! `[OCPI 2.3.0 §mod_locations_location_object]`. So a [`Tariff`] carries one
//! too — [`Tariff::time_zone`] — and every date, weekday and time-of-day
//! restriction is judged against the wall clock that zone puts the period's
//! start on.
//!
//! **Not against the offset the timestamps happen to carry.** An offset is what
//! a clock was written with; a zone is the rule that decides the offset. Judged
//! against the offset, one physical session priced under a German night tariff
//! costs €6.00 when its readings are stamped `+01:00` and €9.00 when the same
//! instants are stamped `Z` — which is the ordinary case, because every session
//! an eMSP re-rates from a roaming partner arrives in UTC.
//!
//! The zone also decides where the *cuts* go, and it knows about the two days a
//! year a civil time is not an instant: a spring gap swallows an hour, so the
//! wall clock passes 02:30 once, at the transition; an autumn fold repeats one,
//! so it passes 02:30 **twice** and both are cut. See
//! [`emob_core::TimeZone::instants_at`].
//!
//! # Rounding happens once, at the end
//!
//! Each line is computed exactly and kept exact. Only [`Rated::total`] and the
//! tax breakdown round, to the currency's minor unit, half away from zero.
//! Rounding per line and then summing gives a different answer, and which of
//! the two is correct is a tax question rather than an arithmetic one — so the
//! exact figures survive and the caller can do either.

use emob_core::quantity::{Currency, Money};
use emob_core::{APPORTIONED_SCALE, Activity, ClockResolution, Energy, Local, TimeZone, apportion};
use rust_decimal::Decimal;

use crate::tariff::{Dimension, PriceComponent, Restrictions, Tariff, TariffElement, TaxIncluded};

/// Seconds in an hour, as a decimal.
const SECONDS_PER_HOUR: Decimal = Decimal::from_parts(3600, 0, 0, false, 0);
/// Watt-hours in a kilowatt-hour, as a decimal.
const WH_PER_KWH: Decimal = Decimal::from_parts(1000, 0, 0, false, 0);
/// Percent, as a decimal.
const HUNDRED: Decimal = Decimal::from_parts(100, 0, 0, false, 0);

/// One slice of a session, in the terms a tariff prices.
///
/// [`Self::activity`] is the distinction `[AFIR Art. 5(4)]` turns on: an
/// occupancy fee is a price for a vehicle that has stopped asking, and folding
/// that together with "no energy flowed" is how a fast charger bills a driver
/// for its own curtailment. See [`Activity`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Period {
    /// When the slice begins.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub start: time::OffsetDateTime,
    /// When it ends.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub end: time::OffsetDateTime,
    /// The energy delivered inside it.
    pub energy: Energy,
    /// What the slice was.
    pub activity: Activity,
}

impl Period {
    /// A period in which energy flowed.
    #[must_use]
    pub const fn charging(
        start: time::OffsetDateTime,
        end: time::OffsetDateTime,
        energy: Energy,
    ) -> Self {
        Self {
            start,
            end,
            energy,
            activity: Activity::Charging,
        }
    }

    /// A period in which the **vehicle** stopped asking for power.
    #[must_use]
    pub const fn parked(start: time::OffsetDateTime, end: time::OffsetDateTime) -> Self {
        Self {
            start,
            end,
            energy: Energy::ZERO,
            activity: Activity::Parked,
        }
    }

    /// A period in which the **operator** stopped offering power while the
    /// vehicle was still asking for it.
    #[must_use]
    pub const fn withheld(start: time::OffsetDateTime, end: time::OffsetDateTime) -> Self {
        Self {
            start,
            end,
            energy: Energy::ZERO,
            activity: Activity::Withheld,
        }
    }

    /// How long the period lasted, in whole seconds. Never negative.
    #[must_use]
    pub fn seconds(&self) -> u64 {
        (self.end - self.start)
            .whole_seconds()
            .max(0)
            .unsigned_abs()
    }

    /// The average power across the period, in kW — `None` for a period with
    /// no duration, because a power restriction cannot be judged against it.
    #[must_use]
    pub fn average_power_kw(&self) -> Option<Decimal> {
        let seconds = self.seconds();
        if seconds == 0 {
            return None;
        }
        Some(self.energy.kwh() * SECONDS_PER_HOUR / Decimal::from(seconds))
    }
}

/// What a session did, period by period — and how finely its clock could
/// tell.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Chargeable {
    periods: Vec<Period>,
    /// The shortest span the station's clock may be billed for.
    ///
    /// `[REA 6-A §3.1]`: "Messwerte unterhalb der kürzest möglichen Zeitspanne
    /// werden nicht für Abrechnungszwecke verwendet." A duration is a measured
    /// value, and the measuring instrument's resolution is a fact about the
    /// session rather than about the tariff — so it travels here, and [`rate`]
    /// reads it. The default is the regulation's cap of sixty seconds, because
    /// a platform that has not read the type approval has not been told the
    /// device is better than the worst case the regulation permits.
    #[cfg_attr(feature = "serde", serde(default))]
    clock: ClockResolution,
}

/// Read back through [`Chargeable::new`], because the ordering and the absence
/// of overlap are what stop a minute being charged twice.
///
/// A derived `Deserialize` restored the periods and asked neither question, so a
/// session read from a store or a partner's document could overlap itself — and
/// [`rate`] prices every period it is given (D264).
#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Chargeable {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        #[derive(serde::Deserialize)]
        struct AsSent {
            periods: Vec<Period>,
            #[serde(default)]
            clock: ClockResolution,
        }

        let sent = AsSent::deserialize(deserializer)?;
        Self::new(sent.periods)
            .map(|chargeable| chargeable.with_clock(sent.clock))
            .map_err(D::Error::custom)
    }
}

impl Chargeable {
    /// Build a chargeable session from its periods.
    ///
    /// # Errors
    ///
    /// [`ChargeableError`] when there are no periods, or when one ends before
    /// it starts, or when two overlap — all of which would let the same minute
    /// be charged twice.
    pub fn new(mut periods: Vec<Period>) -> Result<Self, ChargeableError> {
        if periods.is_empty() {
            return Err(ChargeableError::Empty);
        }
        periods.sort_by_key(|p| p.start);

        for period in &periods {
            if period.end < period.start {
                return Err(ChargeableError::EndsBeforeItStarts {
                    start: period.start,
                });
            }
        }
        for pair in periods.windows(2) {
            if pair[1].start < pair[0].end {
                return Err(ChargeableError::Overlap { at: pair[1].start });
            }
        }

        Ok(Self {
            periods,
            clock: ClockResolution::conforming(),
        })
    }

    /// The same session, measured by a clock whose resolution is known.
    ///
    /// The figure comes from the station's type approval `[REA 6-A §3.1]`; a
    /// station that states none is judged at the regulation's cap.
    #[must_use]
    pub const fn with_clock(mut self, clock: ClockResolution) -> Self {
        self.clock = clock;
        self
    }

    /// The shortest span this session's clock may be billed for.
    #[must_use]
    pub const fn clock(&self) -> ClockResolution {
        self.clock
    }

    /// A session that is one period of delivered energy.
    ///
    /// # Errors
    ///
    /// [`ChargeableError::EndsBeforeItStarts`] when `end` precedes `start`.
    pub fn energy_only(
        energy: Energy,
        start: time::OffsetDateTime,
        end: time::OffsetDateTime,
    ) -> Result<Self, ChargeableError> {
        Self::new(vec![Period::charging(start, end, energy)])
    }

    /// The periods, in time order.
    #[must_use]
    pub fn periods(&self) -> &[Period] {
        &self.periods
    }

    /// When the session started.
    #[must_use]
    pub fn started_at(&self) -> time::OffsetDateTime {
        self.periods[0].start
    }

    /// When it ended.
    #[must_use]
    pub fn ended_at(&self) -> time::OffsetDateTime {
        self.periods[self.periods.len() - 1].end
    }

    /// The energy across every period.
    #[must_use]
    pub fn total_energy(&self) -> Energy {
        self.periods.iter().map(|p| p.energy).sum()
    }

    /// Seconds spent in periods where energy was flowing.
    #[must_use]
    pub fn charging_seconds(&self) -> u64 {
        self.seconds_where(|a| a == Activity::Charging)
    }

    /// Seconds the **vehicle** spent not asking for power — what an occupancy
    /// fee prices, and nothing else.
    #[must_use]
    pub fn parking_seconds(&self) -> u64 {
        self.seconds_where(|a| a == Activity::Parked)
    }

    /// Seconds the **operator** spent not offering power to a vehicle that was
    /// asking for it. Priced by nothing; reported so it is not lost.
    #[must_use]
    pub fn withheld_seconds(&self) -> u64 {
        self.seconds_where(|a| a == Activity::Withheld)
    }

    fn seconds_where(&self, keep: impl Fn(Activity) -> bool) -> u64 {
        self.periods
            .iter()
            .filter(|p| keep(p.activity))
            .map(Period::seconds)
            .sum()
    }
}

/// What can be wrong with a set of periods.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ChargeableError {
    /// No periods at all.
    #[error("a chargeable session needs at least one period")]
    Empty,

    /// A period ends before it starts.
    #[error("the period beginning {start} ends before it starts")]
    EndsBeforeItStarts {
        /// Which period.
        start: time::OffsetDateTime,
    },

    /// Two periods overlap, so a minute would be charged twice.
    #[error("two periods overlap at {at}: the same minute would be charged twice")]
    Overlap {
        /// Where they overlap.
        at: time::OffsetDateTime,
    },
}

/// What the session had done when a period began — what the restrictions read.
///
/// Built with [`SessionState::new`] so that [`Self::local`] and [`Self::at`]
/// cannot disagree: the wall clock a restriction is judged against is derived
/// from the instant, in the tariff's own zone, once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionState {
    /// Energy delivered before this period.
    pub energy_kwh: Decimal,
    /// Seconds elapsed before this period.
    pub elapsed_seconds: u64,
    /// When the period begins.
    pub at: time::OffsetDateTime,
    /// The wall clock [`Tariff::time_zone`] puts [`Self::at`] on.
    ///
    /// What every date, weekday and time-of-day restriction is judged against.
    /// Derived rather than supplied — see [`SessionState::new`].
    pub local: Local,
    /// The average power across this period, if it has a duration.
    pub power_kw: Option<Decimal>,
    /// What is being priced: a charging session (`None`), or a **reservation**
    /// and how it ended.
    ///
    /// `[OCPI 2.3.0 §mod_tariffs_tariffrestrictions_class]` keeps the two apart
    /// with a restriction rather than a dimension, so an element carrying one
    /// never prices a session and an element carrying none never prices a
    /// reservation. Without this field the two are indistinguishable and the
    /// per-dimension rule silently drops one of them — see
    /// [`crate::rate_reservation`].
    pub reserving: Option<ReservationOutcome>,
}

/// What became of a reservation `[OCPI 2.3.0
/// §mod_tariffs_reservation_restriction_type]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ReservationOutcome {
    /// The driver started charging on the reserved point. The reservation ran
    /// from when it was made until the session began.
    Honoured,
    /// It expired unused, and there is no charging session at all — which is
    /// the one case `[OCPI 2.3.0 §mod_cdrs_cdr_object]` lets a CDR omit its
    /// `session_id` for.
    Expired,
}

/// A reservation, in the terms a tariff prices.
///
/// Its window is not the session's: it *"starts when the reservation is made,
/// and ends when the driver starts charging on the reserved EVSE/Location, or
/// when the reservation expires"* `[OCPI 2.3.0
/// §mod_tariffs_tariffrestrictions_class]`. So it has already run before any
/// session begins, and on [`ReservationOutcome::Expired`] there is no session
/// for it to have run before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Reservation {
    /// When the reservation was made.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub from: time::OffsetDateTime,
    /// When it ended — the moment charging began, or the moment it expired.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub to: time::OffsetDateTime,
    /// Which of those two it was.
    pub outcome: ReservationOutcome,
}

impl Reservation {
    /// A reservation the driver used.
    #[must_use]
    pub const fn honoured(from: time::OffsetDateTime, to: time::OffsetDateTime) -> Self {
        Self {
            from,
            to,
            outcome: ReservationOutcome::Honoured,
        }
    }

    /// A reservation that ran out unused.
    #[must_use]
    pub const fn expired(from: time::OffsetDateTime, to: time::OffsetDateTime) -> Self {
        Self {
            from,
            to,
            outcome: ReservationOutcome::Expired,
        }
    }

    /// How long it lasted, in whole seconds. Never negative.
    #[must_use]
    pub fn seconds(&self) -> u64 {
        (self.to - self.from).whole_seconds().max(0).unsigned_abs()
    }
}

impl SessionState {
    /// The state at the start of a session, in a tariff's zone.
    ///
    /// Zero energy, zero elapsed time, no power yet — what
    /// [`crate::display::describe`] asks about and what the first period of a
    /// rating begins from.
    #[must_use]
    pub fn new(zone: &TimeZone, at: time::OffsetDateTime) -> Self {
        Self {
            energy_kwh: Decimal::ZERO,
            elapsed_seconds: 0,
            at,
            local: zone.local(at),
            power_kw: None,
            reserving: None,
        }
    }

    /// The same state, after a session has delivered `energy_kwh` over
    /// `elapsed_seconds`.
    #[must_use]
    pub const fn after(mut self, energy_kwh: Decimal, elapsed_seconds: u64) -> Self {
        self.energy_kwh = energy_kwh;
        self.elapsed_seconds = elapsed_seconds;
        self
    }

    /// The same state, at an average power — `None` for a period with no
    /// duration, against which a power restriction cannot be judged.
    #[must_use]
    pub const fn at_power(mut self, power_kw: Option<Decimal>) -> Self {
        self.power_kw = power_kw;
        self
    }

    /// The same state, pricing a **reservation** rather than a session.
    #[must_use]
    pub const fn reserving(mut self, outcome: ReservationOutcome) -> Self {
        self.reserving = Some(outcome);
        self
    }
}

/// One priced line of a session: one dimension at one price.
///
/// # Two quantities, because one of them cannot be exact
///
/// [`Self::quantity`] is in the unit the price is quoted in — kWh, hours, one
/// session — which is what an invoice line and a driver read. For the two time
/// dimensions that unit is the hour, and **a duration in hours is usually not a
/// decimal**: 3600 has two factors of three, so twenty-five minutes is
/// `0.41666…` and no scale states it. The same arithmetic makes an occupancy
/// fee of €2.50 an hour unshowable per minute under `[AFIR Art. 5(4)]`, met
/// from the other side.
///
/// So [`Self::amount`] is *not* computed from that figure. It is computed from
/// [`Self::base_quantity`] — whole seconds — multiplied by the price before it
/// is divided by 3600, which is exact wherever the arithmetic allows and is
/// what makes €6.00 an hour for twenty-five minutes come out as €2.50 rather
/// than €2.5000000000000000000000000002.
///
/// **The identity that holds is `base_quantity × unit_price / base_units_per_unit
/// == amount`**, and [`Self::reconciles`] checks it. `quantity × unit_price`
/// reproduces the amount only where the conversion terminates, so a billing
/// layer that has to show a quantity and a unit price whose product is the line
/// total has to quote the seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Line {
    /// What was charged for.
    pub dimension: Dimension,
    /// How much of it, in the unit [`Self::unit_price`] is quoted in — kWh,
    /// hours, one session — after any block rounding.
    ///
    /// Carried at the arithmetic's full precision rather than rounded, because
    /// rounding a quantity silently is how a line stops explaining its own
    /// amount. See the type documentation for why it may not be exact.
    pub quantity: Decimal,
    /// The same quantity in the dimension's **base** unit: kWh for energy,
    /// whole seconds for the two time dimensions, one for a flat fee.
    ///
    /// Exact by construction, and the figure [`Self::amount`] was computed
    /// from.
    pub base_quantity: Decimal,
    /// The price per unit that was applied, in the unit [`Self::quantity`] is
    /// stated in.
    pub unit_price: Decimal,
    /// The amount, exact and unrounded.
    pub amount: Decimal,
    /// The VAT percentage, when the component carried one.
    pub vat: Option<Decimal>,
}

impl Line {
    /// How many base units make one of the unit the price is quoted in —
    /// 3600 for the time dimensions, one for everything else.
    #[must_use]
    pub const fn base_units_per_unit(dimension: Dimension) -> Decimal {
        match dimension {
            Dimension::Time | Dimension::ParkingTime => SECONDS_PER_HOUR,
            Dimension::Energy | Dimension::Flat => Decimal::ONE,
        }
    }

    /// Whether the line's own numbers reproduce its amount.
    ///
    /// The invariant a receiving party checks before it disputes a total: the
    /// amount is the base quantity at the unit price, exactly. Asserted in the
    /// crate's tests over every line it produces, because a line that does not
    /// explain its own amount is a line an invoice cannot be built from.
    #[must_use]
    pub fn reconciles(&self) -> bool {
        self.base_quantity * self.unit_price / Self::base_units_per_unit(self.dimension)
            == self.amount
    }
}

/// A minimum or maximum the tariff imposed on the total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Adjustment {
    /// Whether the total was raised or capped.
    pub kind: AdjustmentKind,
    /// What the lines came to before it.
    pub lines_total: Decimal,
    /// The signed amount added to reach the bound. Positive for a minimum,
    /// negative for a maximum.
    pub amount: Decimal,
    /// The VAT rate applied to it.
    ///
    /// Inherited from the largest line by amount, because a minimum charge is
    /// economically more of whatever the session mostly was. It is a choice
    /// rather than a derivation, which is why it is a field a reader can see
    /// and an accountant can argue with, rather than an assumption buried in a
    /// sum.
    pub vat: Option<Decimal>,
}

/// One part of an [`Adjustment`], as it lands in a single VAT category.
///
/// See [`Rated::adjustment_parts`], the only thing that produces one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AdjustmentPart {
    /// The category this part falls in.
    pub vat: Option<Decimal>,
    /// The signed amount in it, in the tariff's own basis. Same sign as
    /// [`Adjustment::amount`], and the parts sum to it exactly.
    pub amount: Decimal,
}

/// Which bound moved the total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum AdjustmentKind {
    /// The total was raised to the tariff's minimum.
    Minimum,
    /// The total was capped at the tariff's maximum.
    Maximum,
}

/// Something the rating had to assume, or refused to assume.
///
/// Serialisable, and that is deliberate: a note travels with the record to the
/// partner who settles against it. "This total was rounded up to a block size"
/// and "this element could not be evaluated" are exactly the facts a settlement
/// dispute turns on, and a note that stays behind in the process that produced
/// it is a note nobody can invoke.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum RatingNote {
    /// A quantity the tariff did not price.
    ///
    /// No element carrying this dimension had restrictions matching the
    /// period — which `[OCPI 2.3.0 §Tariff]` answers with "there will be no
    /// costs for that Tariff Dimension", so it is a price of zero rather than
    /// an error. What makes it worth a note is the amount: a session that
    /// delivered 40 kWh and priced 12 of them is a settlement dispute, and the
    /// number that starts it belongs on the record.
    ///
    /// One note per dimension, however many periods it covered, because
    /// ninety-six identical notes are the same fact reported ninety-six times.
    Unpriced {
        /// Which dimension went unpriced.
        dimension: Dimension,
        /// When the first unpriced period began.
        #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
        at: time::OffsetDateTime,
        /// How many periods it covered.
        periods: usize,
        /// How much went unpriced, in the dimension's **base** unit — kWh,
        /// whole seconds, one session — which is exact.
        base_quantity: Decimal,
    },
    /// An element restricts on **average power**, so this total is a function of
    /// the session *at the resolution its periods carry* rather than of the
    /// session alone.
    ///
    /// Not an error in the arithmetic. Every other restriction reads a quantity
    /// that accumulates, and a period *contains* the fact about where the
    /// threshold was crossed — so [`rate`] can cut there. Average power is
    /// `energy / duration` over whatever window is asked about, and a period
    /// carries no information about the power inside it: 60 kWh in an hour
    /// averages 60 kW, and the same hour as two halves of 55 and 5 kWh averages
    /// 110 and 10. Both are right; the finer one is the better measurement, and
    /// no cut recovers it from the coarser. See the module documentation.
    ///
    /// Reported rather than refused, because `[OCPI 2.3.0 §Tariff]` defines the
    /// restriction and a partner's tariff may carry one. Raised once per
    /// element, from the tariff rather than the session: it is true of the
    /// document before a session arrives.
    PowerJudgedPerPeriod {
        /// Which element, by position.
        index: usize,
        /// The lower bound, when it carries one.
        min_kw: Option<Decimal>,
        /// The upper bound, when it carries one.
        max_kw: Option<Decimal>,
    },
    /// An element was skipped because it carries a restriction this build
    /// cannot evaluate.
    ///
    /// Never silently treated as unrestricted: an element whose conditions
    /// cannot be checked is one whose prices must not be applied.
    UnevaluableRestriction {
        /// Which element, by position.
        index: usize,
        /// The restrictions that could not be judged.
        restrictions: Vec<String>,
    },
    /// A duration was charged for, and the station's clock cannot resolve a
    /// span that short — so it was **not** billed `[REA 6-A §3.1]`.
    ///
    /// "Messwerte unterhalb der kürzest möglichen Zeitspanne werden nicht für
    /// Abrechnungszwecke verwendet." The measured value is the duration, and
    /// below the clock's resolution it is not a number an invoice may use, so
    /// the line is dropped rather than the record refused: the energy is
    /// unaffected, and a thirty-second wait before a charge begins — the
    /// ordinary shape of a transaction that opens `EVConnected` — must not
    /// make fifteen kilowatt-hours unbillable over five cents of occupancy.
    ///
    /// The mirror of an unsynchronised clock, arriving from the other end:
    /// there the clock cannot be *placed*, here the span cannot be *resolved*.
    DurationBelowResolution {
        /// Which time dimension.
        dimension: Dimension,
        /// What the periods measured, in whole seconds.
        measured_seconds: Decimal,
        /// The shortest span the clock may be billed for, in seconds.
        shortest_seconds: Decimal,
    },
    /// The operator withheld power from a vehicle that was asking for it, and
    /// those seconds were priced by nothing.
    ///
    /// Not a fault and not a refusal — it is the correct answer
    /// `[OCPI 2.3.0 §mod_cdrs_chargingperiod_class]` gives, and it is reported
    /// because a session whose clock ran for an hour and whose invoice prices
    /// forty minutes is one somebody will ask about. It is also the number a
    /// `[EnWG §14a]` dimming or a load-management ceiling costs the operator in
    /// time revenue, which is a figure worth having.
    WithheldNotPriced {
        /// How long, in whole seconds.
        seconds: Decimal,
        /// How many periods it was spread over.
        periods: usize,
    },
    /// A quantity was rounded up to the component's block size.
    ///
    /// # The figures are in the dimension's **base** unit
    ///
    /// kWh for energy, whole seconds for the two time dimensions — the unit
    /// [`Line::base_quantity`] is stated in, and the one the difference between
    /// them is exact in. Quoted in the *displayed* unit they were not: a block of
    /// 2100 seconds is `0.5833…` hours, no scale states it, and a reader
    /// reconciling "what was billed" against "what was delivered" was subtracting
    /// two rounded quotients. The one consumer of this note does exactly that
    /// subtraction — `emob_cdr::validate` asks whether the excess over the
    /// record's own quantity is the block this note declares — and an
    /// approximate answer there is a settlement dispute waved through.
    ///
    /// [`Display`](core::fmt::Display) converts to the unit a driver reads, which
    /// is where the division belongs.
    RoundedToBlock {
        /// Which dimension.
        dimension: Dimension,
        /// What was actually used, in the dimension's base unit.
        actual: Decimal,
        /// What was billed, in the same unit. Never below `actual`.
        billed: Decimal,
    },
    /// The total was moved by a minimum or maximum.
    Adjusted(Adjustment),
    /// The tariff's minimum and its maximum both bind on this session, and
    /// they ask for opposite movements.
    ///
    /// `[OCPI 2.3.0 §mod_tariffs_tariff_object]` states both as bounds on
    /// *"a Charging Session with this tariff"*, and a session that is at once
    /// below the floor and above the ceiling is a session the tariff has no
    /// price for. It is a fault in the **document** rather than in the session:
    /// the two figures were written by whoever published the tariff, and no
    /// arithmetic here can reconcile them.
    ///
    /// The maximum is the one applied, because it is the number the driver was
    /// shown as a ceiling and lifting a total above a published ceiling is the
    /// error `[AFIR Art. 5(4)]` and `[PAngV]` exist to prevent. The lift the
    /// minimum asked for is reported so the operator can see what it published.
    LimitsContradict {
        /// What the lines came to before either bound.
        lines_total: Decimal,
        /// The lift the minimum asked for, in the lines' own basis.
        minimum_asks: Decimal,
        /// The cut the maximum asked for, in the same basis. Negative.
        maximum_asks: Decimal,
    },
    /// A maximum asked for a cut deeper than the session's own total, and the
    /// total was held at zero.
    ///
    /// A cap below what the session cost is a discount; a cap below **nothing**
    /// is a payment to the driver, which no tariff states and this crate will
    /// not invent. The figure the tariff asked for is reported beside the one
    /// that was applied.
    AdjustmentClampedAtZero {
        /// What the lines came to.
        lines_total: Decimal,
        /// The movement the bound asked for.
        asked: Decimal,
    },
    /// The bound was deeper than the category it is attributed to, so it is
    /// drawn from more than one.
    ///
    /// EN 16931 gives **one** allowance one tax category — BT-95 and BT-96 —
    /// and `[BR-S-08]` subtracts the whole of it from that category's taxable
    /// amount. A cut deeper than the category holds would take that amount
    /// negative, which no partner and no tax office accepts, so the bound
    /// continues into the next category: BG-20 is repeatable, and
    /// [`Rated::adjustment_parts`] is where the split is stated.
    ///
    /// Reported because it is a **settlement** fact rather than a fault. Which
    /// supplies a cap reduces decides how much tax is due, so a partner
    /// reconciling the record — and an accountant reading the invoice — is
    /// entitled to see that the tariff's own category could not hold it and
    /// which order the rest was drawn in.
    AdjustmentSpread {
        /// The rate the bound is attributed to — the one drawn from first.
        vat: Option<Decimal>,
        /// What the lines at that rate came to.
        in_category: Decimal,
        /// The bound.
        amount: Decimal,
        /// How many categories it was drawn from, this one included.
        categories: usize,
    },
    /// A reservation's window ends before it starts.
    ///
    /// A fault in the document rather than in the arithmetic: nothing here can
    /// know how long the point was actually held. The window is collapsed to
    /// its start, so no minutes are priced and a `FLAT` reservation fee — which
    /// has no duration in it — is still charged.
    ReservationWindowReversed {
        /// When the record says the reservation was made.
        #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
        from: time::OffsetDateTime,
        /// When it says it ended.
        #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
        to: time::OffsetDateTime,
    },
    /// A component carries a VAT rate no net-and-tax split can be computed
    /// from.
    ///
    /// A gross amount is `net × (1 + rate/100)`, so at exactly −100 % the
    /// factor is zero and no net grosses up to it. The amount is still charged
    /// — the price is the price — but [`Rated::tax_summary`] reports it whole,
    /// and an invoice built from it would state a taxable amount it cannot
    /// justify. A tariff from a roaming partner is somebody else's document, so
    /// the rate is data rather than a promise.
    ///
    /// Raised **only for a [`TaxIncluded::Yes`] tariff**, the one basis on which
    /// that factor is a divisor: net prices multiply by it and zero multiplies
    /// fine, and a party outside a tax regime never reads the rate at all.
    VatRateNotUsable {
        /// Which dimension carried it.
        dimension: Dimension,
        /// The rate that arrived.
        rate: Decimal,
    },
}

impl RatingNote {
    /// Whether this note is a term of the price **the payer is being asked to
    /// pay**, rather than a report to the operator about their own document.
    ///
    /// A note concerns the payer exactly when it says **a quantity was billed
    /// differently from how it was measured** — what `[AFIR Art. 5(4)]` and
    /// `[PAngV]` entitle a driver to reconcile, and what a partner disputes.
    /// Everything else is a fault in a document the payer did not write and
    /// cannot act on, and a driver's bill is not where it belongs (D253).
    ///
    /// | Note | Audience |
    /// |---|---|
    /// | [`Self::Unpriced`] | payer — delivered and not charged for |
    /// | [`Self::RoundedToBlock`] | payer — **more** billed than delivered |
    /// | [`Self::DurationBelowResolution`] | payer — a line dropped, and the clock is why |
    /// | [`Self::WithheldNotPriced`] | payer — session time that is not billing time |
    /// | [`Self::Adjusted`] | neither: already an EN 16931 allowance or charge, with its own amount and reason |
    /// | every other variant | operator — a fault in the tariff or the record |
    #[must_use]
    pub const fn concerns_the_payer(&self) -> bool {
        matches!(
            self,
            Self::Unpriced { .. }
                | Self::RoundedToBlock { .. }
                | Self::DurationBelowResolution { .. }
                | Self::WithheldNotPriced { .. }
        )
    }
}

impl core::fmt::Display for RatingNote {
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per variant, and a note's sentence is the whole of \
                  what a settlement dispute is conducted with — splitting the \
                  table by group would hide which variants have a sentence and \
                  which do not"
    )]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unpriced {
                dimension,
                at,
                periods,
                base_quantity,
            } => write!(
                f,
                "no tariff element prices {dimension:?} under the conditions of {periods} period(s) beginning {at}: {base_quantity} {} was not charged",
                dimension.base_unit()
            ),
            Self::PowerJudgedPerPeriod {
                index,
                min_kw,
                max_kw,
            } => write!(
                f,
                "element {index} restricts on average power ({}), which is a property of a period rather than of the session: unlike energy and duration a period carries no information about the power inside it, so there is nothing to cut at and this total is a function of the session at the resolution its periods carry — rate the periods the meter produced, not an average of them",
                match (min_kw, max_kw) {
                    (Some(min), Some(max)) => format!("{min}–{max} kW"),
                    (Some(min), None) => format!("from {min} kW"),
                    (None, Some(max)) => format!("below {max} kW"),
                    (None, None) => "no bound".to_owned(),
                }
            ),
            Self::UnevaluableRestriction {
                index,
                restrictions,
            } => write!(
                f,
                "element {index} carries restrictions this build cannot evaluate and was skipped: {restrictions:?}"
            ),
            Self::DurationBelowResolution {
                dimension,
                measured_seconds,
                shortest_seconds,
            } => write!(
                f,
                "{measured_seconds} s of {dimension} were not billed: the station's clock resolves no span shorter than {shortest_seconds} s, and a measured value below it is not one an invoice may use [REA 6-A §3.1]. The energy is unaffected"
            ),
            Self::WithheldNotPriced { seconds, periods } => write!(
                f,
                "{seconds} s across {periods} period(s) were priced by neither time dimension: the vehicle was asking for power and the point was not offering it, which is not the occupancy [AFIR Art. 5(4)] prices [OCPI 2.3.0 §mod_cdrs_chargingperiod_class]"
            ),
            Self::RoundedToBlock {
                dimension,
                actual,
                billed,
            } => {
                // The note carries base units because that is where the
                // difference is exact; a driver reads hours and kilowatt-hours,
                // so the division happens here and nowhere else.
                let per_unit = Line::base_units_per_unit(*dimension);
                write!(
                    f,
                    "{dimension:?} rounded up from {} to {} {}",
                    actual / per_unit,
                    billed / per_unit,
                    dimension.unit()
                )
            }
            Self::ReservationWindowReversed { from, to } => write!(
                f,
                "the reservation is recorded as running from {from} to {to}, which ends before it starts: no minutes were priced and the window was collapsed to its start"
            ),
            Self::VatRateNotUsable { dimension, rate } => write!(
                f,
                "{dimension:?} carries a VAT rate of {rate} %, from which no net and tax can be computed: the amount is reported whole"
            ),
            Self::Adjusted(adjustment) => match adjustment.kind {
                AdjustmentKind::Minimum => write!(
                    f,
                    "the lines came to {}, raised by {} to the tariff minimum",
                    adjustment.lines_total, adjustment.amount
                ),
                AdjustmentKind::Maximum => write!(
                    f,
                    "the lines came to {}, capped by {} at the tariff maximum",
                    adjustment.lines_total, adjustment.amount
                ),
            },
            Self::LimitsContradict {
                lines_total,
                minimum_asks,
                maximum_asks,
            } => write!(
                f,
                "the tariff's own minimum and maximum both bind on a total of {lines_total}: the minimum asks for {minimum_asks} and the maximum for {maximum_asks} — the maximum was applied, because a published ceiling may not be raised"
            ),
            Self::AdjustmentClampedAtZero { lines_total, asked } => write!(
                f,
                "the tariff maximum asked for {asked} against lines of {lines_total}, which is a total below zero: the adjustment was held at -{lines_total}"
            ),
            Self::AdjustmentSpread {
                vat,
                in_category,
                amount,
                categories,
            } => write!(
                f,
                "the bound of {amount} is deeper than the {in_category} charged at {}, so it is drawn from {categories} VAT categories in the order the largest is drawn from first: one EN 16931 allowance per category (BG-20 is repeatable), rather than one category stating a negative taxable amount",
                match vat {
                    Some(rate) => format!("{rate} % VAT"),
                    None => "no stated VAT rate".to_owned(),
                }
            ),
        }
    }
}

/// One VAT category of a rated session.
///
/// EN 16931 wants the breakdown, not one number: a session whose electricity
/// and whose service fee sit in different categories has two taxable amounts
/// and two tax amounts, and an invoice that states only the gross cannot be
/// checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TaxLine {
    /// The rate, as a percentage. Zero for anything untaxed.
    pub rate: Decimal,
    /// The taxable amount, rounded to the minor unit.
    pub net: Decimal,
    /// The tax on it, rounded to the minor unit.
    pub tax: Decimal,
    /// Net plus tax.
    pub gross: Decimal,
}

/// A rated session.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Rated {
    /// One line per dimension-and-price that applied, in the order
    /// `[AFIR Art. 5(4)]` prescribes.
    pub lines: Vec<Line>,
    /// The currency.
    pub currency: Currency,
    /// Whether the line amounts are gross or net.
    pub tax_included: TaxIncluded,
    /// The minimum or maximum that moved the total, if one did.
    pub adjustment: Option<Adjustment>,
    /// Anything the rating had to assume, or refused to. Travels with the
    /// record.
    pub notes: Vec<RatingNote>,
}

impl Rated {
    /// The sum of the lines, exact and unrounded, before any bound.
    #[must_use]
    pub fn lines_total(&self) -> Decimal {
        self.lines.iter().map(|l| l.amount).sum()
    }

    /// The exact total, before rounding to the minor unit.
    #[must_use]
    pub fn exact_total(&self) -> Money {
        let adjustment = self.adjustment.map_or(Decimal::ZERO, |a| a.amount);
        Money::new(self.lines_total() + adjustment, self.currency)
    }

    /// The total, rounded to the currency's minor unit.
    ///
    /// In the basis the tariff states: gross when
    /// [`TaxIncluded::Yes`], net when [`TaxIncluded::No`]. Use
    /// [`Self::gross`] when the number has to be what the driver pays.
    ///
    /// # This is the tariff's total, not the invoice's
    ///
    /// It rounds **once**, at the end. [`Self::gross`] rounds each VAT category
    /// and sums, because EN 16931 states a taxable and a tax amount per rate
    /// and an invoice's total has to be the sum of what it shows. With one VAT
    /// rate the two agree; with several they can differ by a minor unit, and
    /// the invoice's figure is the one that has to reconcile — so a billing
    /// layer reads [`Self::gross`] and this is what a price quote uses.
    #[must_use]
    pub fn total(&self) -> Money {
        self.exact_total().round_to_minor_unit()
    }

    /// Whether the lines sum to the total.
    ///
    /// False exactly when a minimum or maximum moved it, and then
    /// [`Self::adjustment`] says by how much. There is no other way for the two
    /// to differ: the invariant is that **every term of the total is a line or
    /// the adjustment**.
    #[must_use]
    pub fn lines_sum_to_total(&self) -> bool {
        self.adjustment.is_none()
    }

    /// The amount charged for one dimension, across every price it was charged
    /// at.
    #[must_use]
    pub fn amount_for(&self, dimension: Dimension) -> Option<Decimal> {
        let matching: Vec<Decimal> = self
            .lines
            .iter()
            .filter(|l| l.dimension == dimension)
            .map(|l| l.amount)
            .collect();
        if matching.is_empty() {
            None
        } else {
            Some(matching.into_iter().sum())
        }
    }

    /// The quantity charged for one dimension, across every price, in the unit
    /// the prices are quoted in.
    #[must_use]
    pub fn quantity_for(&self, dimension: Dimension) -> Decimal {
        self.lines
            .iter()
            .filter(|l| l.dimension == dimension)
            .map(|l| l.quantity)
            .sum()
    }

    /// The same, in the dimension's **base** unit — kWh, whole seconds, one —
    /// which is exact.
    ///
    /// What a billing layer aggregates on. Summing [`Self::quantity_for`] over
    /// several time lines accumulates the error of a division by 3600 once per
    /// line; this does not, because there is no division in it.
    #[must_use]
    pub fn base_quantity_for(&self, dimension: Dimension) -> Decimal {
        self.lines
            .iter()
            .filter(|l| l.dimension == dimension)
            .map(|l| l.base_quantity)
            .sum()
    }

    /// What this rating says was **delivered** in one dimension, in the
    /// dimension's base unit — kWh, whole seconds, one session.
    ///
    /// # The identity a price is checked against its record with
    ///
    /// A rating charges for a quantity, and two lawful things stand between that
    /// quantity and the one the meter measured:
    ///
    /// - a `step_size` bills **up to one block more** than was delivered
    ///   `[OCPI 2.3.0 §mod_cdrs_step_size]`, and says so in
    ///   [`RatingNote::RoundedToBlock`];
    /// - a dimension nothing matched is charged **nothing at all** — *"there
    ///   will be no costs for that Tariff Dimension"* `[OCPI 2.3.0 §Tariff]` —
    ///   and says so in [`RatingNote::Unpriced`].
    ///
    /// So the quantity a record has to agree with is neither the billed one nor
    /// the priced one. It is
    /// `base_quantity − block surplus + unpriced`, and this is that figure.
    /// A promotional first tier gives ten kilowatt-hours away and a block size
    /// rounds fifty watt-hours up; both are stated on the record, both are in
    /// this sum, and what is left over is a price computed for a **different
    /// session** — which is what `emob_cdr::validate` blocks and the only thing
    /// it should (D258).
    ///
    /// Exact, because every term is in the base unit: an hour is 3600 seconds
    /// and most durations have no decimal in hours at all.
    #[must_use]
    pub fn accounted_quantity_for(&self, dimension: Dimension) -> Decimal {
        self.base_quantity_for(dimension) - self.block_surplus_for(dimension)
            + self.unpriced_for(dimension)
    }

    /// How much a `step_size` added to one dimension's billed quantity, in the
    /// dimension's base unit. Zero where no block applied.
    ///
    /// Read off [`RatingNote::RoundedToBlock`], which is where the rating states
    /// it — rather than re-derived from the tariff, because the block that
    /// applied is the last relevant component's and a reader of the record does
    /// not have the tariff.
    #[must_use]
    pub fn block_surplus_for(&self, dimension: Dimension) -> Decimal {
        self.notes
            .iter()
            .filter_map(|note| match note {
                RatingNote::RoundedToBlock {
                    dimension: rounded,
                    actual,
                    billed,
                } if *rounded == dimension => Some(billed - actual),
                _ => None,
            })
            .sum()
    }

    /// How much of one dimension no element priced, in the dimension's base
    /// unit. Zero where everything was priced.
    #[must_use]
    pub fn unpriced_for(&self, dimension: Dimension) -> Decimal {
        self.notes
            .iter()
            .filter_map(|note| match note {
                RatingNote::Unpriced {
                    dimension: unpriced,
                    base_quantity,
                    ..
                } if *unpriced == dimension => Some(*base_quantity),
                _ => None,
            })
            .sum()
    }

    /// Whether every line reproduces its own amount from its own numbers.
    ///
    /// True by construction. Re-checkable, because a [`Rated`] can arrive over
    /// the wire inside a CDR somebody else built — and it **is** checked there:
    /// `emob_cdr::validate` asks it of each line rather than of the whole, so
    /// the finding names the line that does not add up. This is the one-line
    /// question, for a caller that only wants the answer.
    #[must_use]
    pub fn lines_reconcile(&self) -> bool {
        self.lines.iter().all(Line::reconciles)
    }

    /// The VAT breakdown, one entry per rate, rounded to the minor unit.
    ///
    /// A line with no VAT percentage, and every line under
    /// [`TaxIncluded::NotApplicable`], falls into the zero-rate entry.
    #[must_use]
    pub fn tax_summary(&self) -> Vec<TaxLine> {
        self.grouped_by_rate(
            self.lines.iter().map(|line| (line.vat, line.amount)).chain(
                self.adjustment_parts()
                    .into_iter()
                    .map(|part| (part.vat, part.amount)),
            ),
        )
    }

    /// The same breakdown for **one dimension's** lines.
    ///
    /// What a wire that states a cost per dimension needs — OCPI's
    /// `total_energy_cost` and its siblings each carry their own tax list. It
    /// is a breakdown rather than one rate for the same reason the whole
    /// summary is: one dimension can be charged at two prices, and a tiered
    /// tariff whose tiers sit in different VAT categories has two taxable
    /// amounts under one heading. Reading the rate off the first line and
    /// applying it to the sum quietly taxes the second tier at the first
    /// tier's rate.
    ///
    /// The adjustment is **not** included. A minimum charge is a term of the
    /// total rather than of any one dimension — [`Adjustment::vat`] records
    /// which category it landed in — and attributing it to a heading here
    /// would make the headings sum to more than the record's own lines.
    #[must_use]
    pub fn tax_summary_for(&self, dimension: Dimension) -> Vec<TaxLine> {
        self.grouped_by_rate(
            self.lines
                .iter()
                .filter(|line| line.dimension == dimension)
                .map(|line| (line.vat, line.amount)),
        )
    }

    /// Where the [`Adjustment`] lands, category by category.
    ///
    /// # A bound is not always one amount in one category
    ///
    /// [`Adjustment::vat`] answers *which* category a bound belongs to — the
    /// largest line's, because a bound is economically more of whatever the
    /// session mostly was. That holds wherever the category has room for it,
    /// which is every single-rate tariff and most others.
    ///
    /// A **maximum** need not have room. A cap of € 3.00 on a session made of
    /// € 5.00 of energy at 19 % and a € 20.00 fee at 7 % takes € 22.00 out of a
    /// category holding € 20.00. As one allowance that category's taxable
    /// amount is **−2.00** — a negative BT-116 under a positive invoice, which
    /// every one of EN 16931's 317 rules accepts and no tax office does (D283).
    ///
    /// The standard's own answer is that BG-20 is **repeatable**: each allowance
    /// carries one category and one rate (BT-95, BT-96), so a bound deeper than
    /// one category is several allowances. This is that split.
    ///
    /// # It is the same order the price was computed against
    ///
    /// The chosen category takes as much as it holds, then the rest in
    /// descending order of what they hold — the same walk the rating solves the
    /// movement along in the first place. A split that used a different order
    /// would state a document the price does not match (D284).
    ///
    /// A **minimum** is always one part: adding to a category cannot drive it
    /// negative.
    ///
    /// ```
    /// # use emob_tariff::{Chargeable, Dimension, Period, PriceComponent, PriceLimit};
    /// # use emob_tariff::{Tariff, TariffElement, TariffKind, TaxIncluded, rate};
    /// # use emob_core::{Currency, Energy, TimeZone};
    /// # use rust_decimal::Decimal;
    /// # use std::str::FromStr;
    /// # use time::macros::datetime;
    /// # let dec = |s: &str| Decimal::from_str(s).unwrap();
    /// # let tariff = Tariff {
    /// #     id: "capped".parse()?,
    /// #     currency: Currency::EUR,
    /// #     kind: TariffKind::Contract,
    /// #     time_zone: TimeZone::new("Europe/Berlin")?,
    /// #     tax_included: TaxIncluded::No,
    /// #     elements: vec![TariffElement::unrestricted(vec![
    /// #         PriceComponent::new(Dimension::Energy, dec("0.50")).with_vat(dec("19")),
    /// #         PriceComponent::new(Dimension::Flat, dec("20.00")).with_vat(dec("7")),
    /// #     ])],
    /// #     min_price: None,
    /// #     max_price: Some(PriceLimit { before_taxes: Some(dec("3.00")), after_taxes: None }),
    /// #     valid_from: None,
    /// #     valid_until: None,
    /// # };
    /// # let session = Chargeable::new(vec![Period::charging(
    /// #     datetime!(2026-06-01 10:00 +2),
    /// #     datetime!(2026-06-01 11:00 +2),
    /// #     Energy::from_kwh(dec("10"))?,
    /// # )])?;
    /// let rated = rate(&tariff, &session);
    /// // € 22.00 off a € 20.00 fee at 7 % and € 5.00 of energy at 19 %.
    /// let parts = rated.adjustment_parts();
    /// assert_eq!(parts.len(), 2);
    /// assert_eq!((parts[0].vat, parts[0].amount), (Some(dec("7")), dec("-20.00")));
    /// assert_eq!((parts[1].vat, parts[1].amount), (Some(dec("19")), dec("-2.00")));
    /// // …and no category is left owing a negative taxable amount.
    /// assert!(rated.tax_summary().iter().all(|t| !t.net.is_sign_negative()));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn adjustment_parts(&self) -> Vec<AdjustmentPart> {
        let Some(adjustment) = self.adjustment else {
            return Vec::new();
        };
        // A minimum only adds, and adding to a category never drives it
        // negative. It is also the one case that arrives with no lines at all,
        // which the walk below would answer with an empty list.
        if !adjustment.amount.is_sign_negative() {
            return vec![AdjustmentPart {
                vat: adjustment.vat,
                amount: adjustment.amount,
            }];
        }

        let mut remaining = -adjustment.amount;
        let mut parts: Vec<AdjustmentPart> = Vec::new();
        for (vat, available) in categories_for_bound(&self.lines, adjustment.vat) {
            if remaining <= Decimal::ZERO {
                break;
            }
            let take = remaining.min(available.max(Decimal::ZERO));
            if take.is_zero() {
                continue;
            }
            parts.push(AdjustmentPart { vat, amount: -take });
            remaining -= take;
        }
        // `bound` clamps a cut at the lines' own total, so nothing is left over
        // for any rating this crate produces. A `Rated` **deserialised** from a
        // partner went through no such clamp (rule 13), and a term that went
        // missing would be a document quietly charging more than it says — so
        // the remainder stays with the category the adjustment names, where
        // `emob-billing` refuses it by name.
        if remaining > Decimal::ZERO {
            match parts.first_mut() {
                Some(first) => first.amount -= remaining,
                None => parts.push(AdjustmentPart {
                    vat: adjustment.vat,
                    amount: -remaining,
                }),
            }
        }
        parts
    }

    /// Group amounts by VAT rate and split each group, in ascending rate order.
    fn grouped_by_rate(
        &self,
        amounts: impl Iterator<Item = (Option<Decimal>, Decimal)>,
    ) -> Vec<TaxLine> {
        let mut groups: Vec<(Decimal, Decimal)> = Vec::new();
        for (rate, amount) in amounts {
            // A party outside a tax regime never reads the rate at all, so
            // every line falls into one zero-rated group.
            let rate = match self.tax_included {
                TaxIncluded::NotApplicable => Decimal::ZERO,
                TaxIncluded::Yes | TaxIncluded::No => rate.unwrap_or(Decimal::ZERO),
            };
            match groups.iter_mut().find(|(r, _)| *r == rate) {
                Some((_, total)) => *total += amount,
                None => groups.push((rate, amount)),
            }
        }

        groups.sort_by_key(|group| group.0);
        groups
            .into_iter()
            .map(|(rate, amount)| self.split_tax(rate, amount))
            .collect()
    }

    /// Split one group's amount into net, tax and gross.
    ///
    /// # The rate that has no split
    ///
    /// A gross amount is `net × (1 + rate/100)`, so recovering the net divides
    /// by that factor — and at a rate of exactly −100 % the factor is zero.
    /// There is no net that grosses up to a non-zero amount at −100 %, so the
    /// question has no answer, and `Decimal`'s division **panics** rather than
    /// saying so. A tariff arriving from a roaming partner is not a document
    /// this crate wrote, so the rate has to be treated as untrusted input: the
    /// amount is reported unsplit, and [`rate`] has already recorded a
    /// [`RatingNote::VatRateNotUsable`] beside it so the fact travels with the
    /// record rather than being swallowed here.
    fn split_tax(&self, rate: Decimal, amount: Decimal) -> TaxLine {
        let round = |d: Decimal| Money::new(d, self.currency).round_to_minor_unit().amount();
        let factor = Decimal::ONE + rate / HUNDRED;
        let (net, gross) = match self.tax_included {
            // The amounts are gross: strip the tax out of them.
            TaxIncluded::Yes if !factor.is_zero() => (round(amount / factor), round(amount)),
            // The amounts are net: add the tax on.
            TaxIncluded::No => (round(amount), round(amount * factor)),
            // …and the rate that cannot be split, reported whole.
            TaxIncluded::Yes | TaxIncluded::NotApplicable => (round(amount), round(amount)),
        };
        TaxLine {
            rate,
            net,
            tax: gross - net,
            gross,
        }
    }

    /// The net and gross totals of the lines, **before** anything is rounded to
    /// the minor unit.
    ///
    /// [`Self::tax_summary`] is the figure a *document* states, and it rounds
    /// each category, because an invoice's total has to be the sum of the
    /// amounts it prints. That is the wrong input for a computation whose
    /// output is itself a term of the exact total: rounding a category to the
    /// nearest cent turns a difference of 10⁻²⁷ in a line into a difference of
    /// a whole cent in what a cap takes off, so the same physical session
    /// priced from a coarser and a finer set of periods comes to two prices
    /// (D245). A price that depends on the granularity of the input is not a
    /// price — the property the whole of [`subdivide_at_thresholds`] exists to
    /// keep — and it was this function's absence that broke it.
    ///
    /// No grouping is needed: the factor is a function of the rate alone, so
    /// summing per line and summing per category are the same sum.
    fn exact_bases(&self) -> (Decimal, Decimal) {
        let mut net = Decimal::ZERO;
        let mut gross = Decimal::ZERO;
        for (rate, amount) in self
            .lines
            .iter()
            .map(|line| (line.vat, line.amount))
            .chain(self.adjustment.map(|a| (a.vat, a.amount)))
        {
            let rate = match self.tax_included {
                TaxIncluded::NotApplicable => Decimal::ZERO,
                TaxIncluded::Yes | TaxIncluded::No => rate.unwrap_or(Decimal::ZERO),
            };
            let factor = Decimal::ONE + rate / HUNDRED;
            match self.tax_included {
                // Gross amounts: strip the tax out. The rate with no split is
                // the one `split_tax` reports rather than dividing into.
                TaxIncluded::Yes if !factor.is_zero() => {
                    net += amount / factor;
                    gross += amount;
                }
                TaxIncluded::No => {
                    net += amount;
                    gross += amount * factor;
                }
                TaxIncluded::Yes | TaxIncluded::NotApplicable => {
                    net += amount;
                    gross += amount;
                }
            }
        }
        (net, gross)
    }

    /// The taxable amount across every category.
    ///
    /// Every term is already rounded to the minor unit; the rounding here only
    /// sets the scale, so a session with no lines at all reads `0.00 EUR`
    /// rather than `0 EUR`.
    #[must_use]
    pub fn net(&self) -> Money {
        Money::new(
            self.tax_summary().iter().map(|t| t.net).sum(),
            self.currency,
        )
        .round_to_minor_unit()
    }

    /// The tax across every category.
    #[must_use]
    pub fn tax(&self) -> Money {
        Money::new(
            self.tax_summary().iter().map(|t| t.tax).sum(),
            self.currency,
        )
        .round_to_minor_unit()
    }

    /// What the driver pays.
    #[must_use]
    pub fn gross(&self) -> Money {
        Money::new(
            self.tax_summary().iter().map(|t| t.gross).sum(),
            self.currency,
        )
        .round_to_minor_unit()
    }

    /// One line per note, for an operator queue.
    pub fn reasons(&self) -> impl Iterator<Item = String> + '_ {
        self.notes.iter().map(ToString::to_string)
    }
}

/// Rate a session against a tariff.
///
/// ```
/// use emob_tariff::{Chargeable, Dimension, PriceComponent, Tariff, TariffKind, rate};
/// use emob_core::{Currency, Energy, TimeZone};
/// use rust_decimal::Decimal;
/// use std::str::FromStr;
/// use time::macros::datetime;
///
/// # let dec = |s: &str| Decimal::from_str(s).unwrap();
/// let tariff = Tariff::simple(
///     "ad-hoc".parse()?,
///     Currency::EUR,
///     TariffKind::AdHoc,
///     TimeZone::new("Europe/Berlin")?,
///     vec![PriceComponent::new(Dimension::Energy, dec("0.49"))],
/// );
///
/// let session = Chargeable::energy_only(
///     Energy::from_kwh(dec("29.500"))?,
///     datetime!(2026-01-02 10:00 +1),
///     datetime!(2026-01-02 10:30 +1),
/// )?;
///
/// let rated = rate(&tariff, &session);
/// assert_eq!(rated.total().to_string(), "14.46 EUR");
/// assert!(rated.lines_sum_to_total());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn rate(tariff: &Tariff, session: &Chargeable) -> Rated {
    rate_with(tariff, session, None)
}

/// Rate a **reservation** against a tariff.
///
/// # A reservation is not a session, and is not priced like one
///
/// It *"starts when the reservation is made, and ends when the driver starts
/// charging on the reserved EVSE/Location, or when the reservation expires"*
/// `[OCPI 2.3.0 §mod_tariffs_tariffrestrictions_class]`, so its clock has
/// already run before any session begins. Only `FLAT` and `TIME` may price it.
///
/// It is a **separate entry point** rather than a period of the session because
/// of the per-dimension rule: a tariff whose unrestricted element prices `TIME`
/// and whose reservation element also prices `TIME` would have the two
/// competing for one slot, and `[OCPI 2.3.0 §Tariff]` would drop one of them
/// without anything failing. The specification keeps them apart with a
/// restriction and a separate `total_reservation_cost`, and so does this.
///
/// # Which elements price it
///
/// A reservation the driver used is priced by the `RESERVATION` elements. One
/// that expired is priced by **both** kinds, in list order, so
/// `RESERVATION_EXPIRES` takes a dimension it states and `RESERVATION` supplies
/// the rest — the specification's own note.
///
/// An expired reservation is priced by the reservation elements and **nothing
/// else**: there is no charging session, so a session fee and a price per kWh
/// have no subject. Its worked example bills € 9.00 and not € 9.50.
///
/// `min_price` and `max_price` do not apply either: they bound *"a Charging
/// Session with this tariff"*, and on an expiry there is no session at all.
///
/// ```
/// use emob_tariff::{Dimension, PriceComponent, Reservation, Restrictions};
/// use emob_tariff::{ReservationRestriction, Tariff, TariffElement, TariffKind};
/// use emob_tariff::{TaxIncluded, rate_reservation};
/// use emob_core::{Currency, TimeZone};
/// use rust_decimal::Decimal;
/// use std::str::FromStr;
/// use time::macros::datetime;
///
/// # let dec = |s: &str| Decimal::from_str(s).unwrap();
/// let tariff = Tariff {
///     id: "with-reservation".parse()?,
///     currency: Currency::EUR,
///     kind: TariffKind::AdHoc,
///     time_zone: TimeZone::new("Europe/Berlin")?,
///     tax_included: TaxIncluded::No,
///     elements: vec![TariffElement {
///         components: vec![PriceComponent::new(Dimension::Time, dec("5.00"))],
///         restrictions: Restrictions {
///             reservation: Some(ReservationRestriction::Reservation),
///             ..Restrictions::default()
///         },
///     }],
///     min_price: None,
///     max_price: None,
///     valid_from: None,
///     valid_until: None,
/// };
///
/// // Reserved at 10:00, plugged in at 10:15.
/// let held = Reservation::honoured(
///     datetime!(2026-01-02 10:00 +1),
///     datetime!(2026-01-02 10:15 +1),
/// );
/// assert_eq!(rate_reservation(&tariff, &held).total().to_string(), "1.25 EUR");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn rate_reservation(tariff: &Tariff, reservation: &Reservation) -> Rated {
    // A window that ends before it starts is a fault in whoever wrote the
    // record, and it is the one input `Chargeable::new` refuses here — a
    // reservation of *no* duration is well-formed and still owes a flat fee.
    //
    // Rated over the collapsed window rather than returned empty: a reversed
    // window prices no minutes, but a `FLAT` reservation fee has no duration in
    // it and is owed either way, and returning an empty `Rated` charged nothing
    // and said nothing — a silent zero for a document that is visibly broken
    // (D250). The note travels with the record so the fault is answerable.
    let mut reversed = None;
    let window = if reservation.to < reservation.from {
        reversed = Some(RatingNote::ReservationWindowReversed {
            from: reservation.from,
            to: reservation.to,
        });
        reservation.from
    } else {
        reservation.to
    };

    // One interval, no energy, and `Activity::Charging` so that the `TIME`
    // dimension is the one it feeds — `PARKING_TIME` is the vehicle's own
    // demand and a reservation has no vehicle on it yet.
    let Ok(window) = Chargeable::new(vec![Period::charging(
        reservation.from,
        window,
        Energy::ZERO,
    )]) else {
        // Unreachable: the only refusals are an empty list and a reversed
        // window, and both are ruled out above.
        return Rated {
            lines: Vec::new(),
            currency: tariff.currency,
            tax_included: tariff.tax_included,
            adjustment: None,
            notes: reversed.into_iter().collect(),
        };
    };
    let mut rated = rate_with(tariff, &window, Some(reservation.outcome));
    rated.notes.extend(reversed);
    rated
}

/// The rating, over a session or over a reservation.
///
/// One function because the arithmetic is one arithmetic — the per-dimension
/// selection, the thresholds, `step_size`, the tax breakdown. Two spellings of
/// it would eventually be two answers, which is the drift this crate exists to
/// prevent.
fn rate_with(
    tariff: &Tariff,
    session: &Chargeable,
    reserving: Option<ReservationOutcome>,
) -> Rated {
    let mut notes = preflight(tariff);

    // One accumulator per (dimension, price, vat): a tiered session charges the
    // same dimension at two prices and the invoice has to show both.
    let mut accumulators: Vec<Accumulator> = Vec::new();
    // What nothing priced, one entry per dimension rather than per period.
    let mut unpriced: Vec<UnpricedTally> = Vec::new();
    let mut cumulative_energy = Decimal::ZERO;
    let mut elapsed_seconds: u64 = 0;
    let mut flat_charged = false;

    // Asked once: the dimensions this tariff prices anywhere, in the order
    // `[AFIR Art. 5(4)]` prescribes. A dimension no element carries is not a
    // dimension this session can be short of.
    let dimensions = tariff.dimensions();

    let periods = subdivide_at_thresholds(tariff, session.periods());
    for (index, period) in periods.iter().enumerate() {
        let mut state = SessionState::new(&tariff.time_zone, period.start)
            .after(cumulative_energy, elapsed_seconds)
            .at_power(period.average_power_kw());
        state.reserving = reserving;
        let seconds = Decimal::from(period.seconds());

        for &dimension in &dimensions {
            // In the dimension's *base* unit: kWh for energy, whole seconds for
            // the two time dimensions, one for a flat fee. Time is accumulated
            // in seconds rather than hours so that the division by 3600 happens
            // once, after the multiplication by the price — 35 minutes at
            // 6.00/h is 3.50 exactly that way and 3.4999999999999999999999999998
            // the other.
            let quantity = match dimension {
                Dimension::Energy => period.energy.kwh(),
                // Which of the two time dimensions a period feeds — and the
                // one case where it feeds neither, because the operator was
                // the party not delivering. See `Activity`.
                Dimension::Time | Dimension::ParkingTime => {
                    if Dimension::pricing(period.activity) == Some(dimension) {
                        seconds
                    } else {
                        Decimal::ZERO
                    }
                }
                Dimension::Flat if flat_charged => Decimal::ZERO,
                Dimension::Flat => Decimal::ONE,
            };
            if quantity.is_zero() {
                continue;
            }

            // The per-dimension question `[OCPI 2.3.0 §Tariff]` asks, rather
            // than "which element matches" — see the module documentation for
            // the tariff shape that makes the difference the whole session's
            // energy.
            if let Some((_, component)) = matching_component(tariff, dimension, &state) {
                if dimension == Dimension::Flat {
                    flat_charged = true;
                }
                accumulate(&mut accumulators, component, quantity, index);
            } else {
                tally_unpriced(&mut unpriced, dimension, period.start, quantity);
            }
        }

        cumulative_energy += period.energy.kwh();
        elapsed_seconds += period.seconds();
    }

    // A flat fee that went unmatched early and matched later was charged, so
    // the tally is a period that was overtaken rather than a fee that was lost.
    if flat_charged {
        unpriced.retain(|tally| tally.dimension != Dimension::Flat);
    }
    notes.extend(unpriced.into_iter().map(|tally| RatingNote::Unpriced {
        dimension: tally.dimension,
        at: tally.at,
        periods: tally.periods,
        base_quantity: tally.base_quantity,
    }));

    // The minutes the operator withheld. Named rather than absent: see
    // `RatingNote::WithheldNotPriced`.
    //
    // Counted off the session's **own** periods rather than off the subdivided
    // ones, and through `Chargeable::withheld_seconds` rather than beside it. A
    // threshold cut divides a period; it does not change what the record says
    // happened in it, so "across how many periods" is a fact about the record
    // and not about this function's working list. The figure had two spellings
    // — the method, which said what it meant and had no caller, and this loop,
    // which had the caller and said it again — and two spellings of one rule is
    // the drift the whole crate exists to prevent (D261).
    let withheld_periods = session
        .periods()
        .iter()
        .filter(|p| p.activity == Activity::Withheld && p.seconds() > 0)
        .count();
    if withheld_periods > 0 {
        notes.push(RatingNote::WithheldNotPriced {
            seconds: Decimal::from(session.withheld_seconds()),
            periods: withheld_periods,
        });
    }

    // `[REA 6-A §3.1]` bounds a **measured value**, and the instrument that
    // measures it is the station's clock. A reservation's window is not one: it
    // ran before the cable went in, no meter observed it, and what stands behind
    // it is the operator's own record of when the point was held. `emob_cdr`
    // already says so where it declines to gate the reservation on the evidence
    // — *"no meter measured it, and the Eichrecht gates are about measured
    // values"* — and this is the same sentence, kept here rather than
    // contradicted: a two-minute reservation was losing its `TIME` line to a
    // sixty-second floor, over a regulation that does not reach it, with a note
    // telling the payer the station's clock was why (D257).
    if reserving.is_none() {
        drop_durations_below_resolution(&mut accumulators, session.clock, &mut notes);
    }

    // `step_size` is a property of the **session**, once per family, and it is
    // applied here rather than per line for that reason
    // `[OCPI 2.3.0 §mod_cdrs_step_size]`. Rounding each price up to its own
    // block over-bills every tiered tariff; rounding each *period* up would
    // bill a two-hour session eight times over. See `apply_step_sizes`.
    apply_step_sizes(&mut accumulators, &mut notes);

    let mut lines: Vec<Line> = accumulators
        .into_iter()
        .filter(|acc| !acc.quantity.is_zero())
        .map(|acc| {
            // Multiply, then divide. `price × seconds / 3600` is exact wherever
            // the arithmetic allows it to be; `price × (seconds / 3600)` has
            // already lost the last digits to a repeating decimal — which is
            // why `base_quantity` is the figure the amount comes from and
            // `quantity` is the one a driver reads.
            let per_display_unit = Line::base_units_per_unit(acc.dimension);
            let billed = acc.quantity;
            Line {
                dimension: acc.dimension,
                quantity: billed / per_display_unit,
                base_quantity: billed,
                unit_price: acc.price,
                amount: billed * acc.price / per_display_unit,
                vat: acc.vat,
            }
        })
        .collect();
    // Stable, so a tier keeps the order it was first charged in, inside the
    // dimension order `[AFIR Art. 5(4)]` prescribes.
    lines.sort_by_key(|l| l.dimension);

    // The bound is computed against the record that already exists, because
    // `[OCPI 2.3.0 §mod_tariffs_pricelimit_class]` states one ceiling before
    // taxes and one after and both bind — so answering it needs the tax
    // breakdown, which is a property of the lines. Built unadjusted, then
    // adjusted: there is no order in which the two can be computed together.
    let mut rated = Rated {
        lines,
        currency: tariff.currency,
        tax_included: tariff.tax_included,
        adjustment: None,
        notes,
    };
    // `min_price` and `max_price` bound "a Charging Session with this tariff"
    // `[OCPI 2.3.0 §mod_tariffs_tariff_object]`, and a reservation is not one.
    if reserving.is_none() {
        let mut extra = Vec::new();
        let adjustment = bound(tariff, &rated, &mut extra);
        rated.notes.append(&mut extra);
        if let Some(adjustment) = adjustment {
            rated.notes.push(RatingNote::Adjusted(adjustment));
            rated.adjustment = Some(adjustment);
        }
    }
    rated
}

/// Drop a time dimension the station's own clock cannot resolve.
///
/// A duration below the shortest billable span is not a measured value an
/// invoice may use `[REA 6-A §3.1]`: *"Messwerte unterhalb der kürzest
/// möglichen Zeitspanne werden nicht für Abrechnungszwecke verwendet."*
///
/// Judged per dimension over what was actually charged for — half a minute of
/// occupancy after a half-hour charge is half a minute, not the session's whole
/// length — and judged on the whole of it rather than per period, because the
/// floor is on the measurement and a session sampled every second still
/// measured its minutes.
fn drop_durations_below_resolution(
    accumulators: &mut Vec<Accumulator>,
    clock: ClockResolution,
    notes: &mut Vec<RatingNote>,
) {
    let shortest = Decimal::from(clock.shortest_billable_span().whole_seconds());
    for dimension in [Dimension::Time, Dimension::ParkingTime] {
        let measured: Decimal = accumulators
            .iter()
            .filter(|acc| acc.dimension == dimension)
            .map(|acc| acc.quantity)
            .sum();
        if !measured.is_zero() && measured < shortest {
            accumulators.retain(|acc| acc.dimension != dimension);
            notes.push(RatingNote::DurationBelowResolution {
                dimension,
                measured_seconds: measured,
                shortest_seconds: shortest,
            });
        }
    }
}

/// What is worth saying about a tariff **before** a session is touched — facts
/// about the document rather than about the arithmetic.
fn preflight(tariff: &Tariff) -> Vec<RatingNote> {
    let mut notes = Vec::new();

    for (index, element) in tariff.elements.iter().enumerate() {
        if !element.restrictions.is_evaluable() {
            notes.push(RatingNote::UnevaluableRestriction {
                index,
                restrictions: element.restrictions.unevaluable.clone(),
            });
        }
        // The one restriction whose answer is not a function of the session.
        // See `RatingNote::PowerJudgedPerPeriod`.
        if element.restrictions.min_power_kw.is_some()
            || element.restrictions.max_power_kw.is_some()
        {
            notes.push(RatingNote::PowerJudgedPerPeriod {
                index,
                min_kw: element.restrictions.min_power_kw,
                max_kw: element.restrictions.max_power_kw,
            });
        }
        // A rate of exactly -100 % makes the gross-to-net factor zero, and no
        // net grosses up to a non-zero amount at that rate. Said once here,
        // rather than discovered by `tax_summary` where there is nowhere to
        // put it.
        //
        // Only on a gross tariff, where the factor is a divisor. Net prices
        // multiply by it and zero multiplies fine; a party outside a tax regime
        // never reads the rate at all.
        if tariff.tax_included == TaxIncluded::Yes {
            for component in &element.components {
                if let Some(rate) = component.vat
                    && (Decimal::ONE + rate / HUNDRED).is_zero()
                {
                    notes.push(RatingNote::VatRateNotUsable {
                        dimension: component.dimension,
                        rate,
                    });
                }
            }
        }
    }

    notes
}

/// Every value in a tariff at which the answer to "which element applies" can
/// change.
struct Thresholds {
    energy: Vec<Decimal>,
    duration: Vec<u64>,
    clock: Vec<time::Time>,
    date: Vec<time::Date>,
}

impl Thresholds {
    /// Whether the tariff restricts on nothing a period can be cut at.
    fn is_empty(&self) -> bool {
        self.energy.is_empty()
            && self.duration.is_empty()
            && self.clock.is_empty()
            && self.date.is_empty()
    }

    /// Gather them once, across every element.
    fn of(tariff: &Tariff) -> Self {
        let mut out = Self {
            energy: Vec::new(),
            duration: Vec::new(),
            clock: Vec::new(),
            date: Vec::new(),
        };
        for element in &tariff.elements {
            let r = &element.restrictions;
            out.energy.extend(r.min_kwh.into_iter().chain(r.max_kwh));
            out.duration
                .extend(r.min_duration_s.into_iter().chain(r.max_duration_s));
            out.clock.extend(r.start_time.into_iter().chain(r.end_time));
            out.date.extend(r.start_date.into_iter().chain(r.end_date));
            // A weekday restriction changes which element applies at **local
            // midnight**, on every day the period spans — and unlike a
            // `start_date`, it names no date to cut at. Without this a session
            // running from Friday 23:00 to Saturday 01:00 in one period is
            // priced for two hours at Friday's rate under a weekday tariff —
            // the one threshold with no value in the tariff to cut at, and
            // invisible, because nothing fails to match.
            //
            // Midnight is a `start_time`-shaped cut rather than a date-shaped
            // one, so it goes in with the clock thresholds and the day walk in
            // `clock_cut_offsets` puts one at the start of every day the period
            // touches.
            if !r.days_of_week.is_empty() {
                out.clock.push(time::Time::MIDNIGHT);
            }
        }
        out
    }
}

/// Cut every period wherever a tariff threshold falls inside it.
///
/// # Why this is not the caller's job
///
/// A tariff's energy and duration restrictions are read against what the
/// session has done **so far**, which is what makes tiers tier. Judged only at
/// the start of each period, the tier boundary lands wherever the caller's
/// periods happen to land: hand [`rate`] one period of 15 kWh under "the first
/// 10 kWh at 0.39, the rest at 0.59" and all fifteen are charged at 0.39, and
/// hand it the same session as three periods of five and it charges ten and
/// five correctly. A price that depends on the granularity of the input is not
/// a price.
///
/// So the thresholds themselves become the cut points, and the answer stops
/// depending on how the session was sliced.
///
/// **The wall clock is one of them**, and in this market the common one: "0.30
/// from 22:00" restricts *when the period is* and fails the same way judged only
/// at a period's start. [`Restrictions::start_time`], [`Restrictions::end_time`],
/// the midnight a [`Restrictions::start_date`] or [`Restrictions::end_date`]
/// turns on, and — because a weekday changes at midnight and names no date to
/// cut at — every local midnight a [`Restrictions::days_of_week`] period spans,
/// all cut on equal footing with the kilowatt-hours. Read off the local clock
/// the period carries, because that is the frame [`matches_restrictions`] judges
/// them in, and on every day the period spans.
///
/// # What is divided exactly, and what is divided proportionally
///
/// **Energy is exact at an energy threshold.** A cut at a 10 kWh threshold puts
/// exactly 10 kWh before it, whatever the arithmetic of the surrounding period —
/// and the sub-periods' energies are differences of cumulative values, so they
/// telescope back to the original total to the last digit, the same construction
/// the quarter-hour split uses.
///
/// **Time is exact at a clock threshold**, for the mirror-image reason: a 22:00
/// boundary is 22:00, and it is the energy either side of it that is
/// apportioned. Splitting a quarter hour at a kilowatt-hour boundary — or a
/// kilowatt-hour at a clock boundary — assumes constant power across it, which a
/// tapering charge curve does not deliver; the residual is under a second of a
/// per-minute fee, and the alternative — sub-second period boundaries — would
/// lose whole seconds to `whole_seconds()` and stop the durations summing.
///
/// **And the energy is exact even where the second is not there to hold it.** A
/// period can be shorter than the time it takes to cross a threshold — a second
/// of a 350 kW charge is a tenth of a kilowatt-hour — and then the instant the
/// register passes it rounds onto the period's own start or end. The cut is
/// placed anyway, on the register: the slice it opens is degenerate in time and
/// exact in kilowatt-hours, because the energy is the quantity being tiered and
/// a boundary dropped for want of a second reprices the whole slice in the tier
/// it began in (D221). Where the period has an interior second the cut takes it,
/// so both pieces keep a duration and an average power a restriction can be
/// judged against.
fn subdivide_at_thresholds(tariff: &Tariff, periods: &[Period]) -> Vec<Period> {
    let thresholds = Thresholds::of(tariff);
    if thresholds.is_empty() {
        return periods.to_vec();
    }

    let mut out = Vec::with_capacity(periods.len());
    let mut cumulative_energy = Decimal::ZERO;
    let mut elapsed_seconds: u64 = 0;

    for period in periods {
        let seconds = period.seconds();
        let ceiling = cumulative_energy + period.energy.kwh();
        let cuts = usable_cuts(
            &cuts_inside(
                tariff,
                &thresholds,
                period,
                cumulative_energy,
                elapsed_seconds,
            ),
            seconds,
            cumulative_energy,
            ceiling,
        );
        divide(period, &cuts, cumulative_energy, ceiling, &mut out);
        cumulative_energy = ceiling;
        elapsed_seconds += seconds;
    }

    // The property the whole function rests on: cutting a session divides it,
    // it does not change it.
    //
    // Compared at `APPORTIONED_SCALE`, and the reason is `Decimal`'s own
    // mantissa rather than this arithmetic. Every energy the workspace produces
    // is quoted to that scale or coarser — `emob_core::apportion` is what makes
    // that true — and there the two sums are equal digit for digit. A caller
    // may hand over a period whose energy already spends all ninety-six bits on
    // a repeating fraction, and two of *those* cannot be added exactly by any
    // arithmetic: the pieces would then differ from the whole in the last place
    // and no code here would be at fault.
    debug_assert_eq!(
        out.iter()
            .map(|p| p.energy)
            .sum::<Energy>()
            .kwh()
            .round_dp(APPORTIONED_SCALE),
        periods
            .iter()
            .map(|p| p.energy)
            .sum::<Energy>()
            .kwh()
            .round_dp(APPORTIONED_SCALE),
        "subdividing at thresholds must conserve the session's energy"
    );
    out
}

/// Every threshold that falls inside one period, as an offset in whole seconds
/// from its start and the cumulative register value there.
///
/// Unsorted and unfiltered — two thresholds can land in the same second and
/// disagree about how much had been delivered by it, which is [`usable_cuts`]'s
/// business.
fn cuts_inside(
    tariff: &Tariff,
    thresholds: &Thresholds,
    period: &Period,
    cumulative_energy: Decimal,
    elapsed_seconds: u64,
) -> Vec<(u64, Decimal)> {
    let seconds = period.seconds();
    let energy = period.energy.kwh();
    let mut cuts: Vec<(u64, Decimal)> = Vec::new();

    // A period with no duration cannot be cut in time, and one that delivered
    // nothing has no energy boundary inside it.
    if seconds == 0 {
        return cuts;
    }

    // A duration threshold and a wall-clock threshold are the same kind of cut
    // — an offset into the period — and the energy at either is apportioned the
    // same way.
    let by_offset = |offset: u64, cuts: &mut Vec<(u64, Decimal)>| {
        if offset > 0 && offset < seconds {
            cuts.push((
                offset,
                apportion(cumulative_energy, energy, offset, seconds),
            ));
        }
    };
    for &threshold in &thresholds.duration {
        if let Some(offset) = threshold.checked_sub(elapsed_seconds) {
            by_offset(offset, &mut cuts);
        }
    }
    for offset in clock_cut_offsets(
        &tariff.time_zone,
        &thresholds.clock,
        &thresholds.date,
        period,
    ) {
        by_offset(offset, &mut cuts);
    }

    // An energy threshold is exact: the cut puts the threshold itself either
    // side, whatever the arithmetic of the surrounding period.
    if energy > Decimal::ZERO {
        for &threshold in &thresholds.energy {
            let delta = threshold - cumulative_energy;
            if delta > Decimal::ZERO && delta < energy {
                cuts.push((seconds_for(delta, energy, seconds), threshold));
            }
        }
    }
    cuts
}

/// The cuts that leave both the clock and the register running forwards, in
/// order, each dividing something.
///
/// An energy threshold and a duration threshold can land in the same second and
/// disagree about how much had been delivered by it, and a piece whose energy
/// ran backwards would be a negative quantity — so the sweep keeps the first of
/// any such pair and drops the rest. The window it gives up is under a second
/// either way.
///
/// # The clock may stand still where the register does not
///
/// An energy threshold falls at an instant, and in a period short enough — a
/// second of a 350 kW charge, a slice cut at a state change — that instant
/// rounds onto the period's own start or end. Requiring the cut to advance the
/// clock dropped it, and the whole period was then priced in the tier it began
/// in: a two-second slice carrying the fourth kilowatt-hour of "the first 4 kWh
/// at 0.30" charged all of it at 0.30, and the same session sliced more coarsely
/// did not — a price that depends on the granularity of the input, which is the
/// failure the cuts exist to remove (D221).
///
/// So a cut that divides the *energy* is kept even where it divides no second,
/// and the slice it opens is degenerate in time and exact in kilowatt-hours. The
/// energy is the quantity being tiered; the second it fell in is not one the
/// period can resolve, and rounding it away is the residual `[REA 6-A §3.1]`
/// already bounds.
///
/// # …and what such a slice cannot be priced by
///
/// A slice with no duration has no average power, so an element restricting on
/// one cannot price it — [`matches_restrictions`] refuses rather than guessing,
/// and the energy is reported as [`RatingNote::Unpriced`] with its quantity and
/// its instant.
///
/// That is the trade taken deliberately. The alternative is the behaviour this
/// replaced: drop the cut for want of a second, and price the whole period in
/// the tier it began in — silently, at a total that depends on how finely the
/// input happened to be sliced. A quantity nobody priced is a line somebody
/// answers for; a quantity priced in the wrong tier is not, and the note beside
/// it (`RatingNote::PowerJudgedPerPeriod`) already says that a power restriction
/// makes the total a function of the resolution.
fn usable_cuts(
    cuts: &[(u64, Decimal)],
    seconds: u64,
    cumulative_energy: Decimal,
    ceiling: Decimal,
) -> Vec<(u64, Decimal)> {
    let mut sorted = cuts.to_vec();
    sorted.sort_unstable();

    let mut boundary = (0_u64, cumulative_energy);
    sorted.retain(|&(offset, at_cut)| {
        let forwards =
            offset >= boundary.0 && offset <= seconds && at_cut >= boundary.1 && at_cut <= ceiling;
        // A cut closing neither a second nor a watt-hour opens an empty slice,
        // and one landing on the period's own end with nothing left over is the
        // final piece, which is emitted anyway.
        let divides = offset > boundary.0 || at_cut > boundary.1;
        let interior = offset < seconds || at_cut < ceiling;
        let usable = forwards && divides && interior;
        if usable {
            boundary = (offset, at_cut);
        }
        usable
    });
    sorted
}

/// One period as the slices its cuts divide it into.
///
/// The energies are **differences of cumulative values**, so the pieces
/// telescope back to the period's own energy exactly. The last piece ends at the
/// period's own end rather than at a whole-second offset, for the same reason —
/// and a period nothing cuts is passed through untouched, because `seconds`
/// truncates and rebuilding a whole period out of whole seconds would lose the
/// sub-second remainder of one that has one.
fn divide(
    period: &Period,
    cuts: &[(u64, Decimal)],
    cumulative_energy: Decimal,
    ceiling: Decimal,
    out: &mut Vec<Period>,
) {
    if cuts.is_empty() {
        out.push(period.clone());
        return;
    }

    // Non-negative by the sweep; the clamp is unreachable and is here so this
    // cannot become a panic if that ever changes.
    let slice = |from: Decimal, to: Decimal| {
        Energy::from_kwh((to - from).max(Decimal::ZERO)).unwrap_or(Energy::ZERO)
    };

    let mut previous = (0_u64, cumulative_energy);
    for &(offset, at_cut) in cuts {
        out.push(Period {
            start: period.start + time::Duration::seconds(i64_of(previous.0)),
            end: period.start + time::Duration::seconds(i64_of(offset)),
            energy: slice(previous.1, at_cut),
            activity: period.activity,
        });
        previous = (offset, at_cut);
    }
    out.push(Period {
        start: period.start + time::Duration::seconds(i64_of(previous.0)),
        end: period.end,
        energy: slice(previous.1, ceiling),
        activity: period.activity,
    });
}

/// The offsets, in whole seconds from the period's start, at which a wall-clock
/// restriction changes which element applies.
///
/// Computed in the **tariff's own zone**, because that is the frame
/// [`matches_restrictions`] judges the times and dates in — a cut in any other
/// would split a period into two pieces that price identically and leave the
/// real boundary uncut. Every local day the period spans is walked: an
/// overnight session crosses `22:00` and `06:00` on different dates.
///
/// # A clock change is not an edge case here, it is a Sunday
///
/// A civil time is not an instant. On the autumn fold the wall clock passes
/// `02:30` twice, an hour apart, and a night tariff switching there switches
/// twice — so [`TimeZone::instants_at`] returns both and both are cut. On the
/// spring gap it passes once, at the transition itself. Getting this wrong
/// leaves an hour of one Sunday a year priced at the wrong rate, which is the
/// kind of error that is never found because nobody re-reads a bill from the
/// last weekend in October.
fn clock_cut_offsets(
    zone: &TimeZone,
    times: &[time::Time],
    dates: &[time::Date],
    period: &Period,
) -> Vec<u64> {
    // A corruption guard rather than a rule — a period spanning more than a year
    // is a clock fault, and the same bound the session split refuses one at.
    const MAX_DAYS: u16 = 366;

    if times.is_empty() && dates.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut push_if_inside = |candidate: time::OffsetDateTime| {
        if candidate > period.start
            && candidate < period.end
            && let Ok(seconds) = u64::try_from((candidate - period.start).whole_seconds())
        {
            out.push(seconds);
        }
    };

    // A date restriction turns on at local midnight of the date it names. The
    // `push_if_inside` filter does the rest, so a date outside the period costs
    // one lookup and contributes nothing.
    for &date in dates {
        for candidate in zone.instants_at(date, time::Time::MIDNIGHT) {
            push_if_inside(candidate);
        }
    }

    // Every day the period touches, in the tariff zone's own calendar.
    let last = zone.local(period.end).date;
    let mut date = zone.local(period.start).date;
    for _ in 0..MAX_DAYS {
        for &clock in times {
            for candidate in zone.instants_at(date, clock) {
                push_if_inside(candidate);
            }
        }
        if date >= last {
            break;
        }
        let Some(next) = date.next_day() else { break };
        date = next;
    }

    out
}

/// How many whole seconds into a period `delta` kWh of its `energy` lands.
///
/// Kept **inside** the period wherever the period has an inside — a whole
/// second strictly between its ends. The caller has already established that
/// the threshold falls within the period's energy, so the boundary is real; it
/// is the second it fell in that a coarse clock cannot name, and pushing it to
/// the nearest interior one moves the boundary by under a second while leaving
/// both pieces with a duration, and therefore with an average power a
/// restriction can be judged against.
///
/// A period of one second or less has no interior, and there the answer is 0 or
/// `seconds`: the piece is degenerate in time and exact in kilowatt-hours, which
/// is the trade the cut is worth making (D221).
fn seconds_for(delta: Decimal, energy: Decimal, seconds: u64) -> u64 {
    use rust_decimal::prelude::ToPrimitive as _;
    let offset = (Decimal::from(seconds) * delta / energy)
        .round()
        .to_u64()
        .unwrap_or(0)
        .clamp(0, seconds);
    if seconds >= 2 {
        offset.clamp(1, seconds - 1)
    } else {
        offset
    }
}

/// Seconds as the signed count `time::Duration` takes. A period longer than
/// `i64::MAX` seconds cannot exist — `SplitError::ImplausiblyLong` refuses one
/// at a year — so the saturation is unreachable rather than lossy.
fn i64_of(seconds: u64) -> i64 {
    i64::try_from(seconds).unwrap_or(i64::MAX)
}

/// A dimension charged at one price, accumulating across periods.
///
/// `quantity` is in the dimension's **base** unit — kWh for energy, seconds for
/// the two time dimensions, one for a flat fee — which is also the unit
/// `step_size` is expressed in for time. The conversion to the displayed unit
/// happens once, at the end, after the multiplication by the price.
struct Accumulator {
    dimension: Dimension,
    price: Decimal,
    vat: Option<Decimal>,
    step_size: u32,
    quantity: Decimal,
    /// The index of the last period that fed this accumulator.
    ///
    /// `[OCPI 2.3.0 §mod_cdrs_step_size]` rounds a session's total with the
    /// step size of "the last relevant Price Component", which on a tariff that
    /// switches back — day, night, day again — is not the last accumulator in
    /// this list. So the order is recorded rather than inferred from position.
    last_period: usize,
}

/// One dimension's unpriced quantity, accumulating across periods.
struct UnpricedTally {
    dimension: Dimension,
    at: time::OffsetDateTime,
    periods: usize,
    base_quantity: Decimal,
}

/// Record that a period's quantity in one dimension found no price.
fn tally_unpriced(
    into: &mut Vec<UnpricedTally>,
    dimension: Dimension,
    at: time::OffsetDateTime,
    quantity: Decimal,
) {
    if let Some(existing) = into.iter_mut().find(|t| t.dimension == dimension) {
        existing.periods += 1;
        // A flat fee is one fee however many periods failed to match it.
        // Everything else accumulates, because that is the quantity a dispute
        // is about.
        if dimension != Dimension::Flat {
            existing.base_quantity += quantity;
        }
    } else {
        into.push(UnpricedTally {
            dimension,
            at,
            periods: 1,
            base_quantity: quantity,
        });
    }
}

fn accumulate(
    into: &mut Vec<Accumulator>,
    component: &PriceComponent,
    quantity: Decimal,
    period: usize,
) {
    if let Some(existing) = into.iter_mut().find(|a| {
        a.dimension == component.dimension && a.price == component.price && a.vat == component.vat
    }) {
        existing.quantity += quantity;
        // A tariff that prices the same dimension twice at one price with two
        // block sizes is a tariff whose author meant the larger one.
        existing.step_size = existing.step_size.max(component.step_size);
        existing.last_period = period;
    } else {
        into.push(Accumulator {
            dimension: component.dimension,
            price: component.price,
            vat: component.vat,
            step_size: component.step_size,
            quantity,
            last_period: period,
        });
    }
}

/// The VAT rate a minimum or maximum inherits.
///
/// # Two cases, and the second is the one that bites
///
/// Normally it is the rate of the **largest line by amount**, because a minimum
/// charge is economically more of whatever the session mostly was. That is a
/// choice rather than a derivation, which is why [`Adjustment::vat`] is a field
/// a reader can see and an accountant can argue with.
///
/// But a minimum charge is at its most load-bearing on a session with **no
/// lines at all** — a driver who plugged in, drew nothing and left — and there
/// the largest line does not exist. Falling back to `None` there put the whole
/// minimum charge in the zero-rate group: a €0.50 minimum on a 19 % tariff came
/// out as €0.50 net and €0.00 tax, which is an invoice that under-declares its
/// own VAT.
///
/// So with no lines the rate comes from the tariff's own components, when they
/// agree on one — [`Tariff::vat_basis`], which is the same question the OCPI and
/// OCPP crossings ask, asked through the same function rather than reimplemented
/// here. Where the components state nothing, or disagree, there is no rate
/// anybody wrote down and `None` is the honest answer: a tariff mixing rates and
/// charging a minimum for nothing has a question for its author.
fn adjustment_vat(tariff: &Tariff, lines: &[Line]) -> Option<Decimal> {
    if let Some(largest) = lines.iter().max_by_key(|l| l.amount.abs()) {
        return largest.vat;
    }
    tariff.vat_basis().stated()
}

/// The categories a bound is drawn from, **largest first**, with the one
/// [`adjustment_vat`] chose at the front.
///
/// One walk, shared by the two things that must agree about it: [`bound`], which
/// solves how far a limit moves the total, and [`Rated::adjustment_parts`],
/// which says where the movement lands. Two spellings of this order would be a
/// document that states a different split from the one the price was computed
/// against — the drift this crate exists to prevent, at the last seam it has
/// left (D284).
fn categories_for_bound(
    lines: &[Line],
    chosen: Option<Decimal>,
) -> Vec<(Option<Decimal>, Decimal)> {
    let mut held: Vec<(Option<Decimal>, Decimal)> = Vec::new();
    for line in lines {
        match held.iter_mut().find(|(vat, _)| *vat == line.vat) {
            Some((_, total)) => *total += line.amount,
            None => held.push((line.vat, line.amount)),
        }
    }
    // Descending by what each holds, then the chosen one to the front. A
    // category holding nothing is left in: it contributes no room and taking
    // it out would make the order depend on the amounts twice.
    held.sort_by(|(_, left), (_, right)| right.cmp(left));
    if let Some(index) = held.iter().position(|(vat, _)| *vat == chosen) {
        let front = held.remove(index);
        held.insert(0, front);
    }
    held
}

/// What one unit of movement in the lines' own basis does to the **other**
/// basis, for a movement that lands in the category `vat` names.
///
/// `None` where the question has no answer: a gross amount is
/// `net × (1 + rate/100)`, so at exactly −100 % the factor is zero and no net
/// grosses up to it. The limb that needed it does not bind, which is what this
/// function returning `None` makes the caller do — the same hole
/// [`Rated::split_tax`] reports rather than dividing into.
fn per_unit_in_other_basis(basis: TaxIncluded, vat: Option<Decimal>) -> Option<Decimal> {
    let factor = Decimal::ONE + vat.unwrap_or(Decimal::ZERO) / HUNDRED;
    match basis {
        // Net lines: one net unit moves the gross by the factor.
        TaxIncluded::No => (factor > Decimal::ZERO).then_some(factor),
        // Gross lines: one gross unit moves the net by its reciprocal.
        TaxIncluded::Yes => (factor > Decimal::ZERO).then(|| Decimal::ONE / factor),
        // Outside a tax regime the two totals are one figure.
        TaxIncluded::NotApplicable => Some(Decimal::ONE),
    }
}

/// How far the total has to move, in the lines' **own** basis, to move the
/// *other* basis by `wanted`.
///
/// Signed the way the movement is: a lift is positive, a cut negative.
///
/// # A lift is one factor and a cut is a walk
///
/// A lift lands entirely in the category the bound is attributed to
/// ([`adjustment_vat`]), because adding to a category can never take it below
/// zero — so one factor answers it, which is the division this used to be.
///
/// A cut deeper than that category's own lines continues into the next, and the
/// next has a different rate. The other basis is therefore **piecewise-linear**
/// in how deep the cut goes, with a slope change at each category boundary, and
/// the inversion walks [`categories_for_bound`] spending each category's room
/// until the remainder fits inside the one it has reached. That is the same
/// walk [`Rated::adjustment_parts`] then places the movement along, so the
/// price and the document are computed against one order (D284).
///
/// `None` where the other basis has no answer at all: a rate of exactly −100 %
/// makes the gross-to-net factor zero, the hole [`Rated::split_tax`] reports
/// rather than dividing into. Asked of **every** category rather than of the
/// one a solve happens to stop in, because which it stops in is what the solve
/// is for.
fn movement_reaching(
    wanted: Decimal,
    basis: TaxIncluded,
    categories: &[(Option<Decimal>, Decimal)],
    chosen: Option<Decimal>,
) -> Option<Decimal> {
    if !wanted.is_sign_negative() {
        let per = per_unit_in_other_basis(basis, chosen)?;
        return (!per.is_zero()).then(|| wanted / per);
    }
    if !categories
        .iter()
        .all(|(vat, _)| per_unit_in_other_basis(basis, *vat).is_some())
    {
        return None;
    }

    let mut remaining = -wanted;
    let mut movement = Decimal::ZERO;
    for (vat, available) in categories {
        let available = (*available).max(Decimal::ZERO);
        if available.is_zero() {
            continue;
        }
        let per = per_unit_in_other_basis(basis, *vat)?;
        let segment = available * per;
        if remaining <= segment {
            // `per` is strictly positive here: a zero factor would have failed
            // the check above.
            return Some(-(movement + remaining / per));
        }
        movement += available;
        remaining -= segment;
    }
    // Cutting everything does not reach the target. The deepest cut there is,
    // which the caller's clamp then reads as "the whole of the lines".
    Some(-movement)
}

/// Apply the tariff's minimum and maximum, **in both bases**.
///
/// # Two ceilings, not one ceiling in two spellings
///
/// `[OCPI 2.3.0 §mod_tariffs_pricelimit_class]` states a bound before taxes and
/// a bound after taxes, and says they bind separately:
///
/// > As the taxes on a Charging Session might be different for different parts
/// > of the Session, there might be situations where the maximum cost after
/// > taxes is reached earlier or later than the maximum price before taxes. So
/// > as a rule, **they both apply**.
///
/// A tariff with one VAT rate makes the two proportional and the distinction
/// invisible. A tariff with two — a session fee at the standard rate beside
/// energy at a reduced one — does not, and then a single net ceiling lets the
/// gross total past the gross ceiling. On the specification's own `max_price`
/// example that is five cents over a maximum the operator published.
///
/// So each limb is turned into the movement it needs **in the basis the lines
/// are quoted in**, and the binding one is the largest movement for a minimum
/// and the smallest for a maximum. The limb that matches the tariff's own basis
/// is exact, because it is a subtraction.
///
/// # …and the other limb is a *piecewise* conversion, not a division
///
/// Reaching the other basis needs to know what a unit of movement does to it,
/// and that is a property of the **category the movement lands in**. A lift
/// lands in one — [`adjustment_vat`]'s answer, a field a reader can see — so
/// one factor answers it.
///
/// A **cut** need not. Where it is deeper than that category's own lines it
/// continues into the next, and the next one has a different rate: the gross
/// effect of a net cut is piecewise-linear in how deep the cut goes, with a
/// slope change at each category boundary. Dividing by one factor there answers
/// a gross ceiling with a movement that does not reach it — a price above a
/// maximum the operator published, which is the failure this whole function
/// exists to prevent, one layer down (D284).
///
/// So the other limb is **inverted along [`categories_for_bound`]** — the same
/// walk [`Rated::adjustment_parts`] then uses to place the movement, so the
/// price and the document are computed against one order rather than two. With
/// a single VAT rate, or a cut that fits in its own category, the walk has one
/// segment and the arithmetic is the division it always was.
fn bound(tariff: &Tariff, rated: &Rated, notes: &mut Vec<RatingNote>) -> Option<Adjustment> {
    let lines_total = rated.lines_total();
    let vat = adjustment_vat(tariff, &rated.lines);

    // The totals the two limbs are read against, before any adjustment.
    // `rated.adjustment` is `None` here by construction.
    //
    // Exact rather than the document's rounded categories: what this function
    // returns is a term of the exact total, and a cent of category rounding on
    // the way in is a cent on the price. See `Rated::exact_bases`.
    let (net, gross) = rated.exact_bases();

    // Which limb the lines are already quoted in — that one is exact, because
    // reaching it is a subtraction.
    let (own_is_net, other_total) = match tariff.tax_included {
        TaxIncluded::No => (true, gross),
        TaxIncluded::Yes => (false, net),
        // Outside a tax regime the two totals are one figure.
        TaxIncluded::NotApplicable => (true, lines_total),
    };

    // The order a cut is drawn in, and what each category holds. Computed once:
    // it is a fact about the lines, and both limbs of both limits read it.
    let categories = categories_for_bound(&rated.lines, vat);

    // What each limb asks for, in the lines' own basis.
    let movements = |limit: crate::tariff::PriceLimit| -> Vec<Decimal> {
        let (own_target, other_target) = if own_is_net {
            (limit.before_taxes, limit.after_taxes)
        } else {
            (limit.after_taxes, limit.before_taxes)
        };
        let mut out = Vec::new();
        if let Some(target) = own_target {
            out.push(target - lines_total);
        }
        if let Some(target) = other_target
            && let Some(movement) =
                movement_reaching(target - other_total, tariff.tax_included, &categories, vat)
        {
            out.push(movement);
        }
        out
    };

    // # Every limb is a bound on the movement itself
    //
    // Each limb states a target for a total, and every total here is the lines'
    // own total plus the movement times a constant — so "reach this target" is
    // "move by at least/at most this much", and the four limbs of the two
    // limits are four bounds on one number. The minimum's limbs give a **floor**
    // on the movement, the maximum's a **ceiling**, and the answer is the
    // movement closest to zero inside the interval they leave.
    //
    // Written this way the two are answered together. They used to be answered
    // in order — a minimum that lifted returned before the maximum was looked
    // at, and a maximum that cut returned without the minimum being read — and
    // each was right about its own limb while the pair was a price the tariff
    // does not state: a session lifted **above** the published maximum, or cut
    // **below** the minimum it charges (D247). Sign-filtering each limit's
    // limbs hid the second case entirely, because a floor only shows itself as
    // a lift once the cut has already been taken.
    let floor = tariff
        .min_price
        .and_then(|limit| movements(limit).into_iter().max());
    let ceiling = tariff
        .max_price
        .and_then(|limit| movements(limit).into_iter().min());

    // An interval with nothing in it is a tariff contradicting itself, and the
    // reading that stands is the one that is not against the customer: a
    // published ceiling is never raised.
    if let (Some(floor), Some(ceiling)) = (floor, ceiling)
        && floor > ceiling
    {
        notes.push(RatingNote::LimitsContradict {
            lines_total,
            minimum_asks: floor,
            maximum_asks: ceiling,
        });
    }
    let amount = match (floor, ceiling) {
        // Below the floor: lift to it. The ceiling, where there is one, has
        // already been found to be no lower.
        (Some(floor), _) if floor > Decimal::ZERO && ceiling.is_none_or(|c| floor <= c) => floor,
        // Above the ceiling: cut to it. This is also the contradiction's answer.
        (_, Some(ceiling)) if ceiling < Decimal::ZERO || floor.is_some_and(|f| f > ceiling) => {
            ceiling
        }
        // Inside the interval, so the lines already state a lawful price.
        _ => return None,
    };

    // A session cannot cost less than nothing. A maximum below what the other
    // tax categories alone come to would otherwise take the total negative,
    // which is not a discount — it is a payment to the driver that no tariff
    // asked for.
    let amount = if lines_total + amount < Decimal::ZERO {
        notes.push(RatingNote::AdjustmentClampedAtZero {
            lines_total,
            asked: amount,
        });
        -lines_total
    } else {
        amount
    };
    if amount.is_zero() {
        return None;
    }

    // Where the category the bound is attributed to cannot hold the whole of
    // it, the rest is drawn from the next — `adjustment_parts`, walking the
    // order this function has just solved against. That is a settlement fact
    // rather than a fault, and it travels with the record because which
    // supplies a cap reduces decides how much tax is due.
    let in_category: Decimal = rated
        .lines
        .iter()
        .filter(|line| line.vat == vat)
        .map(|line| line.amount)
        .sum();
    if (in_category + amount).is_sign_negative() {
        // Counted off the same walk rather than re-derived: the split is one
        // rule, and a note that counted categories its own way would be a
        // second reading of it.
        let spread = Rated {
            adjustment: Some(Adjustment {
                kind: AdjustmentKind::Maximum,
                lines_total,
                amount,
                vat,
            }),
            lines: rated.lines.clone(),
            currency: rated.currency,
            tax_included: rated.tax_included,
            notes: Vec::new(),
        };
        notes.push(RatingNote::AdjustmentSpread {
            vat,
            in_category,
            amount,
            categories: spread.adjustment_parts().len(),
        });
    }

    Some(Adjustment {
        kind: if amount.is_sign_negative() {
            AdjustmentKind::Maximum
        } else {
            AdjustmentKind::Minimum
        },
        lines_total,
        amount,
        vat,
    })
}

/// Apply `step_size` the way `[OCPI 2.3.0 §mod_cdrs_step_size]` says it is
/// applied: **once per session per family**, never once per price.
///
/// > When calculating the cost of a charging session, `step_size` SHALL only be
/// > taken into account **once per session** for the `TariffDimensionType`
/// > `ENERGY` and **once for `PARKING_TIME` and `TIME` combined**.
///
/// Two families, then. Rounding each *price* up to its own block is the reading
/// a tiered tariff makes expensive: the specification's own worked example —
/// 4.3 kWh at € 0.20 and 1.1 at € 0.27, both in 500 Wh blocks — costs €1.31 that
/// way and €1.18 this way, an eleven per cent over-charge in the direction
/// `[AFIR Art. 5(4)]` and `[PAngV]` care about.
///
/// Within a family the total takes the block of *"the last relevant Price
/// Component"*, and the surplus is billed at that component's **price** rather
/// than spread — which makes the worked session 25 minutes at €1.20 and 20 at
/// €2.40 rather than 30 and 30.
///
/// The time family carries one more rule: where `TIME` and `PARKING_TIME` are
/// both used, `step_size` applies *"only … for the total parking duration"*,
/// because the charging time "is not rounded up, as it is followed by another
/// time based period". So the family rounds the dimension it **ended** on — and
/// that is why this takes the accumulators rather than one quantity, and why
/// they carry the period they were last fed by.
fn apply_step_sizes(accumulators: &mut [Accumulator], notes: &mut Vec<RatingNote>) {
    round_family(accumulators, &[Dimension::Energy], notes);
    round_family(
        accumulators,
        &[Dimension::Time, Dimension::ParkingTime],
        notes,
    );
}

/// Round one family's total up to the block of the price component it ended on.
///
/// `FLAT` is in no family: `[OCPI 2.3.0 §mod_tariffs_tariffdimensiontype_enum]`
/// gives it no `step_size` unit to multiply.
fn round_family(
    accumulators: &mut [Accumulator],
    family: &[Dimension],
    notes: &mut Vec<RatingNote>,
) {
    // The dimension of the family this session ended on — the only one the
    // block applies to when the family has two.
    let Some(last) = accumulators
        .iter()
        .enumerate()
        .filter(|(_, a)| family.contains(&a.dimension) && !a.quantity.is_zero())
        .max_by_key(|(_, a)| a.last_period)
        .map(|(index, _)| index)
    else {
        return;
    };
    let dimension = accumulators[last].dimension;
    let step_size = accumulators[last].step_size;
    if step_size <= 1 {
        return;
    }

    let per_base_unit = match dimension {
        Dimension::Energy => WH_PER_KWH,
        Dimension::Time | Dimension::ParkingTime => Decimal::ONE,
        Dimension::Flat => return,
    };
    let exact: Decimal = accumulators
        .iter()
        .filter(|a| a.dimension == dimension)
        .map(|a| a.quantity)
        .sum();

    // # A block boundary is not crossed by arithmetic noise
    //
    // The ceiling is the one step in this crate that turns an arbitrarily small
    // difference into an arbitrarily large one: a quantity a hair above a whole
    // number of blocks is billed a whole block more than one exactly on it. And
    // the hair is there — `subdivide_at_thresholds` divides a period's energy at
    // a threshold, and `Decimal` carries ninety-six bits, so the same physical
    // session summed from a coarser and a finer set of periods lands 10⁻²⁷ kWh
    // apart. Fed to `ceil` unrounded that is 51 Wh on the invoice, charged
    // against the customer, for a difference no meter could state (D246).
    //
    // So the ceiling is taken on the quantity at `APPORTIONED_SCALE` — a
    // nanowatt-hour, the scale at which this crate already declares two
    // slicings of one session to be the same session, and nine orders of
    // magnitude finer than the milliwatt-hour an OCMF register states.
    let measured = exact.round_dp(APPORTIONED_SCALE);
    let step = Decimal::from(step_size);
    let billed = (measured * per_base_unit / step).ceil() * step / per_base_unit;
    if billed == measured {
        return;
    }

    // The surplus is billed at the last price, not spread across the tiers —
    // and it is measured from `exact`, so the family's billed total is the
    // whole number of blocks `billed` names and carries no residue forward.
    accumulators[last].quantity += billed - exact;

    // The note carries the **base** unit — kWh, whole seconds — because the
    // difference between the two figures is what a reader reconciles against,
    // and 2100 seconds is `0.5833…` hours in every scale there is. `Display`
    // converts to the unit a driver reads.
    notes.push(RatingNote::RoundedToBlock {
        dimension,
        actual: measured,
        billed,
    });
}

/// The price component that prices one dimension in a given session state, and
/// the index of the element it came from.
///
/// `[OCPI 2.3.0 §Tariff]`: "the first Tariff Element with a Price Component for
/// that dimension in the list with matching Tariff Restrictions will be used".
/// Elements that price other dimensions are stepped over rather than stopped
/// at — which is the difference between reading a two-element `{FLAT}`,
/// `{ENERGY}` tariff correctly and billing a session's fee without its
/// kilowatt-hours.
///
/// Public so [`crate::display`] selects by *this* rule rather than a parallel
/// one. Two implementations of "which price applies" is exactly the drift this
/// crate exists to prevent, one level down.
#[must_use]
pub fn matching_component<'a>(
    tariff: &'a Tariff,
    dimension: Dimension,
    state: &SessionState,
) -> Option<(usize, &'a PriceComponent)> {
    tariff.elements.iter().enumerate().find_map(|(index, e)| {
        // The dimension first, the restrictions second: an element that does
        // not price this dimension is not a candidate at all, so whether its
        // restrictions match is not a question worth asking.
        let component = e.component(dimension)?;
        element_matches(e, state).then_some((index, component))
    })
}

/// Whether an element's restrictions admit a session state.
///
/// Public so [`crate::display`] selects the element by *this* rule rather than
/// a parallel one. Two implementations of "which element applies" is exactly
/// the drift this crate exists to prevent, one level down.
#[must_use]
pub fn element_matches(element: &TariffElement, state: &SessionState) -> bool {
    matches_restrictions(&element.restrictions, state)
}

fn matches_restrictions(r: &Restrictions, state: &SessionState) -> bool {
    // An element whose conditions cannot be checked is one whose prices must
    // not be applied. Treating it as unrestricted would apply a night rate at
    // noon on the strength of a field this build did not understand.
    if !r.is_evaluable() {
        return false;
    }

    // Asked before anything else, because it decides *what* is being priced
    // rather than *when*. `[OCPI 2.3.0 §mod_tariffs_tariffrestrictions_class]`:
    // "When this field is present, the Tariff Element describes reservation
    // costs" — so the two populations do not overlap in either direction.
    #[allow(
        clippy::match_same_arms,
        reason = "one arm per rule of the specification; collapsing them by \
                  return value would make the table unreadable"
    )]
    match (r.reservation, state.reserving) {
        // An ordinary session, priced by ordinary elements.
        (None, None) => {}
        // A reservation element never prices a session, and a session element
        // never prices a reservation: a tariff whose unrestricted element
        // carries `TIME` would otherwise have the reservation's minutes and the
        // session's charging minutes compete for one dimension.
        (Some(_), None) | (None, Some(_)) => return false,
        // A reservation the driver used is priced by `RESERVATION` only.
        (
            Some(crate::tariff::ReservationRestriction::ReservationExpires),
            Some(ReservationOutcome::Honoured),
        ) => return false,
        // …and one that expired is priced by both kinds, in list order, so
        // `RESERVATION_EXPIRES` takes the dimension where a tariff states both:
        // "then the time based cost of an expired reservation will be
        // calculated based on the `RESERVATION_EXPIRES` Tariff Element".
        (Some(_), Some(_)) => {}
    }

    if r.is_unrestricted() {
        return true;
    }

    if r.min_kwh.is_some_and(|min| state.energy_kwh < min)
        || r.max_kwh.is_some_and(|max| state.energy_kwh >= max)
    {
        return false;
    }

    if r.min_duration_s
        .is_some_and(|min| state.elapsed_seconds < min)
        || r.max_duration_s
            .is_some_and(|max| state.elapsed_seconds >= max)
    {
        return false;
    }

    if r.min_power_kw.is_some() || r.max_power_kw.is_some() {
        // A period with no duration has no power, and a power restriction
        // cannot be judged against it.
        let Some(power) = state.power_kw else {
            return false;
        };
        if r.min_power_kw.is_some_and(|min| power < min)
            || r.max_power_kw.is_some_and(|max| power >= max)
        {
            return false;
        }
    }

    // Every one of these is a statement about the wall clock at the charge
    // point, so every one of them reads the local civil value the tariff's own
    // zone puts this instant at — never the offset the timestamp was written
    // with. See the module documentation.
    let date = state.local.date;
    if r.start_date.is_some_and(|from| date < from) || r.end_date.is_some_and(|to| date >= to) {
        return false;
    }

    if !r.days_of_week.is_empty() && !r.days_of_week.contains(&state.local.weekday) {
        return false;
    }

    // `end_time` is exclusive, and midnight is the one value where that reading
    // is wrong: `[OCPI 2.3.0 §mod_tariffs_tariffrestrictions_class]` says "to
    // stop at end of the day use: 00:00". Read as an exclusive instant, `00:00`
    // ends the window before it opens and the element matches nothing at all —
    // and an element that matches nothing prices nothing, silently, on a tariff
    // whose author wrote what the specification told them to write.
    //
    // So `00:00` as an *end* means the end of the day. With a `start_time` the
    // wrap-around arm already reached that answer; alone it did not, and a
    // `{end_time: "00:00"}` element left a whole session `Unpriced`.
    let ends_at_midnight = r.end_time == Some(time::Time::MIDNIGHT);
    let clock = state.local.time;
    match (r.start_time, r.end_time) {
        // "Until the end of the day", however it was written.
        (Some(from), Some(_)) if ends_at_midnight => {
            if clock < from {
                return false;
            }
        }
        (None, Some(_)) if ends_at_midnight => {}
        (Some(from), Some(to)) if from <= to => {
            // An ordinary window inside one day.
            if clock < from || clock >= to {
                return false;
            }
        }
        (Some(from), Some(to)) => {
            // A window that wraps midnight — 22:00 to 06:00. Treating this as
            // an empty range is the classic night-tariff bug.
            if clock < from && clock >= to {
                return false;
            }
        }
        (Some(from), None) if clock < from => return false,
        (None, Some(to)) if clock >= to => return false,
        _ => {}
    }

    true
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_reversed_reservation_window_still_owes_its_flat_fee_and_says_so() {
        // A partner's record with `to` before `from`. This used to return an
        // empty `Rated`: no fee, no note, and a total of zero on a document
        // that is visibly broken.
        let tariff = Tariff {
            id: "r".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: TimeZone::new("Europe/Berlin").unwrap(),
            tax_included: TaxIncluded::No,
            elements: vec![TariffElement {
                components: vec![
                    PriceComponent::new(Dimension::Flat, dec("2.00")),
                    PriceComponent::new(Dimension::Time, dec("6.00")),
                ],
                restrictions: Restrictions {
                    reservation: Some(crate::tariff::ReservationRestriction::Reservation),
                    ..Restrictions::default()
                },
            }],
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        };

        let reversed = Reservation::honoured(
            time::macros::datetime!(2026-01-02 10:30 +1),
            time::macros::datetime!(2026-01-02 10:00 +1),
        );
        let rated = rate_reservation(&tariff, &reversed);

        assert_eq!(
            rated.total().to_string(),
            "2.00 EUR",
            "the fee is still owed"
        );
        assert_eq!(
            rated.quantity_for(Dimension::Time),
            Decimal::ZERO,
            "and no minutes are invented"
        );
        assert!(
            rated
                .notes
                .iter()
                .any(|n| matches!(n, RatingNote::ReservationWindowReversed { .. })),
            "{:?}",
            rated.notes
        );
    }
    use super::*;
    use crate::tariff::PriceLimit;
    use crate::tariff::{TariffKind, TaxIncluded};
    use rust_decimal::prelude::FromStr;
    use time::macros::datetime;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn kwh(s: &str) -> Energy {
        Energy::from_kwh(dec(s)).unwrap()
    }

    fn at(minute: i64) -> time::OffsetDateTime {
        datetime!(2026-01-02 10:00 +1) + time::Duration::minutes(minute)
    }

    fn ad_hoc(components: Vec<PriceComponent>) -> Tariff {
        Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            TimeZone::new("Europe/Berlin").unwrap(),
            components,
        )
    }

    fn tiered(elements: Vec<TariffElement>) -> Tariff {
        Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            tax_included: TaxIncluded::Yes,
            elements,
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        }
    }

    fn session(energy: &str) -> Chargeable {
        Chargeable::energy_only(kwh(energy), at(0), at(30)).unwrap()
    }

    #[test]
    fn energy_is_rated_exactly() {
        let t = ad_hoc(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        let r = rate(&t, &session("29.500"));

        assert_eq!(r.lines.len(), 1);
        assert_eq!(r.lines[0].amount, dec("14.45500"));
        assert_eq!(r.exact_total().amount(), dec("14.45500"));
        assert_eq!(r.total().to_string(), "14.46 EUR");
        assert!(r.lines_sum_to_total());
    }

    #[test]
    fn a_span_the_clock_cannot_resolve_is_not_billed_and_the_energy_is() {
        // `[REA 6-A §3.1]`: "Messwerte unterhalb der kürzest möglichen
        // Zeitspanne werden nicht für Abrechnungszwecke verwendet." Thirty
        // seconds of occupancy against the regulation's sixty-second cap is a
        // measured value an invoice may not use, so the line goes and the
        // kilowatt-hours stay.
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
        ]);
        let s = Chargeable::new(vec![
            Period::charging(at(0), at(30), kwh("15")),
            Period::parked(at(30), at(30) + time::Duration::seconds(30)),
        ])
        .unwrap();

        let r = rate(&t, &s);
        assert_eq!(r.amount_for(Dimension::ParkingTime), None);
        assert_eq!(r.amount_for(Dimension::Energy), Some(dec("7.35")));
        assert!(
            r.notes.iter().any(|n| matches!(
                n,
                RatingNote::DurationBelowResolution {
                    dimension: Dimension::ParkingTime,
                    ..
                }
            )),
            "{:?}",
            r.notes
        );
        assert!(r.reasons().any(|n| n.contains("REA 6-A")));

        // A station whose type approval states a better figure bills it.
        let precise = ClockResolution::stated(time::Duration::seconds(10)).unwrap();
        let r = rate(&t, &s.clone().with_clock(precise));
        assert_eq!(r.amount_for(Dimension::ParkingTime), Some(dec("0.05")));
        assert!(r.notes.is_empty(), "{:?}", r.notes);

        // …and the floor is on the measurement as a whole, not on each period:
        // sixty seconds measured as two half-minutes is still a minute.
        let sampled = Chargeable::new(vec![
            Period::charging(at(0), at(30), kwh("15")),
            Period::parked(at(30), at(30) + time::Duration::seconds(30)),
            Period::parked(at(30) + time::Duration::seconds(30), at(31)),
        ])
        .unwrap();
        assert_eq!(
            rate(&t, &sampled).amount_for(Dimension::ParkingTime),
            Some(dec("0.10"))
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn a_session_arrives_through_the_constructor_that_refuses_an_overlap() {
        // Two periods covering one minute is that minute charged twice, and
        // `Chargeable::new` refuses it. A derived `Deserialize` restored the
        // list and asked nothing, and `rate` prices every period it is given
        // (D264).
        let overlapping = r#"{"periods":[
            {"start":"2026-01-02T10:00:00+01:00","end":"2026-01-02T10:30:00+01:00",
             "energy":"10.000","activity":"charging"},
            {"start":"2026-01-02T10:15:00+01:00","end":"2026-01-02T10:45:00+01:00",
             "energy":"10.000","activity":"charging"}]}"#;
        assert!(serde_json::from_str::<Chargeable>(overlapping).is_err());
        assert!(serde_json::from_str::<Chargeable>(r#"{"periods":[]}"#).is_err());

        // …and an ordinary session still reads, with the clock its record
        // states — the figure a replay may not be told a different one of.
        let ordinary = r#"{"periods":[
            {"start":"2026-01-02T10:00:00+01:00","end":"2026-01-02T10:30:00+01:00",
             "energy":"10.000","activity":"charging"}],"clock":10}"#;
        let session: Chargeable = serde_json::from_str(ordinary).unwrap();
        assert_eq!(session.periods().len(), 1);
        assert_eq!(
            session.clock().shortest_billable_span(),
            time::Duration::seconds(10)
        );
    }

    #[test]
    fn a_reservation_is_not_judged_against_the_stations_metering_clock() {
        // `[REA 6-A §3.1]` bounds a **measured value**, and the instrument is
        // the station's clock. A reservation ran before the cable went in and no
        // meter observed it — `emob_cdr` says so where it declines to gate the
        // reservation on the evidence — so the sixty-second floor does not reach
        // it. It was reaching it: a driver who reserved a point and plugged in
        // forty-five seconds later had the whole `TIME` line dropped, with a note
        // telling them the station's clock was why (D257).
        let t = Tariff {
            id: "with-reservation".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: TimeZone::new("Europe/Berlin").unwrap(),
            tax_included: TaxIncluded::No,
            elements: vec![TariffElement {
                components: vec![PriceComponent::new(Dimension::Time, dec("5.00"))],
                restrictions: Restrictions {
                    reservation: Some(crate::tariff::ReservationRestriction::Reservation),
                    ..Restrictions::default()
                },
            }],
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        };

        let brief = Reservation::honoured(at(0), at(0) + time::Duration::seconds(45));
        let rated = rate_reservation(&t, &brief);
        assert_eq!(
            rated.total().to_string(),
            "0.06 EUR",
            "45 s at 5.00/h, charged"
        );
        assert!(
            !rated
                .notes
                .iter()
                .any(|n| matches!(n, RatingNote::DurationBelowResolution { .. })),
            "{:?}",
            rated.notes
        );

        // …and the floor still governs the session beside it, on the same
        // tariff object, because that duration *is* a measured value.
        let session = Chargeable::new(vec![
            Period::charging(at(0), at(30), kwh("15")),
            Period::parked(at(30), at(30) + time::Duration::seconds(45)),
        ])
        .unwrap();
        let occupancy = Tariff::simple(
            "occupancy".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(Dimension::ParkingTime, dec("5.00"))],
        );
        assert!(
            rate(&occupancy, &session)
                .notes
                .iter()
                .any(|n| matches!(n, RatingNote::DurationBelowResolution { .. })),
        );
    }

    #[test]
    fn a_block_rounding_states_its_figures_in_the_unit_a_difference_is_exact_in() {
        // The note is read by `emob_cdr::validate`, which subtracts the block
        // from what was billed to find out what was delivered. In hours that
        // subtraction is two rounded quotients — 2100 s is `0.5833…` h in every
        // scale there is — so the figures are base units and the division to the
        // unit a driver reads happens in `Display` (D258).
        let t = Tariff::simple(
            "blocks".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(Dimension::ParkingTime, dec("6.00")).with_step_size(3600)],
        );
        let s = Chargeable::new(vec![Period::parked(at(0), at(35))]).unwrap();
        let rated = rate(&t, &s);

        let note = rated
            .notes
            .iter()
            .find_map(|n| match n {
                RatingNote::RoundedToBlock { actual, billed, .. } => Some((*actual, *billed)),
                _ => None,
            })
            .expect("the block rounded");
        assert_eq!(note, (dec("2100"), dec("3600")), "whole seconds, exactly");
        assert_eq!(
            rated.block_surplus_for(Dimension::ParkingTime),
            dec("1500"),
            "and the difference is exact, which it is not in hours"
        );
        // The sentence a driver reads is still in hours.
        assert!(
            rated.reasons().any(|r| r.ends_with(" h")),
            "{:?}",
            rated.reasons().collect::<Vec<_>>()
        );

        // The identity the record is checked against: what was billed, less the
        // block, plus what nothing priced, is what was there.
        assert_eq!(
            rated.accounted_quantity_for(Dimension::ParkingTime),
            dec("2100")
        );
    }

    #[test]
    fn what_a_rating_says_was_delivered_is_what_it_charged_plus_what_it_gave_away() {
        // "The first 10 kWh are free, then 0.49" — written the way `[OCPI 2.3.0
        // §Tariff]`'s per-dimension rule invites, with no unrestricted energy
        // element behind it. The rating charges twenty of thirty kilowatt-hours
        // and reports the ten it did not, and the sum of the two is the session.
        //
        // Before this identity existed, `emob_cdr::validate` compared the billed
        // figure alone against the record and **blocked** it — so a lawful
        // promotional tariff produced a record the builder emitted and the
        // validator refused, and `emob-billing` would not invoice the month
        // (D258).
        let t = Tariff {
            id: "promo".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: TimeZone::new("Europe/Berlin").unwrap(),
            tax_included: TaxIncluded::Yes,
            elements: vec![TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.49"))],
                restrictions: Restrictions {
                    min_kwh: Some(dec("10")),
                    ..Restrictions::default()
                },
            }],
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        };
        let s = Chargeable::energy_only(kwh("30.000"), at(0), at(60)).unwrap();
        let rated = rate(&t, &s);

        assert_eq!(rated.base_quantity_for(Dimension::Energy), dec("20.000"));
        assert_eq!(rated.unpriced_for(Dimension::Energy), dec("10.000"));
        assert_eq!(rated.block_surplus_for(Dimension::Energy), Decimal::ZERO);
        assert_eq!(
            rated.accounted_quantity_for(Dimension::Energy),
            dec("30.000"),
            "the whole session is accounted for: charged, or named as unpriced"
        );
        assert_eq!(rated.total().to_string(), "9.80 EUR");
    }

    #[test]
    fn a_tiered_tariff_charges_each_tier_at_its_own_price() {
        // "The first 10 kWh at 0.39, the rest at 0.59" — rated against the
        // session total, a 30 kWh session would be repriced entirely at 0.59,
        // including the ten kilowatt-hours the driver was quoted at 0.39.
        let t = tiered(vec![
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.39"))],
                restrictions: Restrictions {
                    max_kwh: Some(dec("10")),
                    ..Restrictions::default()
                },
            },
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.59"))]),
        ]);

        // Three ten-kilowatt-hour periods: the first crosses into the second
        // tier, the rest sit in it.
        let s = Chargeable::new(vec![
            Period::charging(at(0), at(15), kwh("10")),
            Period::charging(at(15), at(30), kwh("10")),
            Period::charging(at(30), at(45), kwh("10")),
        ])
        .unwrap();

        let r = rate(&t, &s);
        assert_eq!(r.lines.len(), 2, "two prices, two lines: {:?}", r.lines);
        assert_eq!(r.lines[0].unit_price, dec("0.39"));
        assert_eq!(r.lines[0].quantity, dec("10"));
        assert_eq!(r.lines[1].unit_price, dec("0.59"));
        assert_eq!(r.lines[1].quantity, dec("20"));
        assert_eq!(r.exact_total().amount(), dec("15.70")); // 3.90 + 11.80
        assert_eq!(
            r.quantity_for(Dimension::Energy),
            dec("30"),
            "and the energy still conserves across the tiers"
        );
    }

    #[test]
    fn a_tier_boundary_inside_a_period_is_cut_at_the_threshold() {
        // The whole session as one period. Asking only at the start of it, the
        // first tier matches and all thirty kilowatt-hours are charged at 0.39
        // — a price that depends on how the caller happened to slice the
        // session, which is not a price.
        let t = tiered(vec![
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.39"))],
                restrictions: Restrictions {
                    max_kwh: Some(dec("10")),
                    ..Restrictions::default()
                },
            },
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.59"))]),
        ]);

        let one_period = Chargeable::new(vec![Period::charging(at(0), at(45), kwh("30"))]).unwrap();
        let r = rate(&t, &one_period);

        assert_eq!(r.lines.len(), 2, "{:?}", r.lines);
        assert_eq!(r.lines[0].quantity, dec("10"), "exactly at the threshold");
        assert_eq!(r.lines[0].unit_price, dec("0.39"));
        assert_eq!(r.lines[1].quantity, dec("20"));
        assert_eq!(r.lines[1].unit_price, dec("0.59"));
        assert_eq!(r.exact_total().amount(), dec("15.70"));
    }

    #[test]
    fn the_same_session_costs_the_same_however_it_is_sliced() {
        // The property that makes the cut worth making: one period, three
        // periods, or ninety-six quarter hours all come to the same money, and
        // the energy still conserves across the tiers.
        let t = tiered(vec![
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.39"))],
                restrictions: Restrictions {
                    max_kwh: Some(dec("10")),
                    ..Restrictions::default()
                },
            },
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.49"))],
                restrictions: Restrictions {
                    max_kwh: Some(dec("25")),
                    ..Restrictions::default()
                },
            },
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.59"))]),
        ]);

        let expected = rate(
            &t,
            &Chargeable::new(vec![Period::charging(at(0), at(48), kwh("36"))]).unwrap(),
        )
        .exact_total()
        .amount();
        // 10 × 0.39 + 15 × 0.49 + 11 × 0.59 = 3.90 + 7.35 + 6.49
        assert_eq!(expected, dec("17.74"));

        for slices in [2_i64, 3, 4, 6, 12, 16, 48] {
            let step = 48 / slices;
            let periods: Vec<Period> = (0..slices)
                .map(|i| {
                    Period::charging(
                        at(i * step),
                        at((i + 1) * step),
                        kwh(&(dec("36") / Decimal::from(slices)).to_string()),
                    )
                })
                .collect();
            let r = rate(&t, &Chargeable::new(periods).unwrap());
            assert_eq!(
                r.exact_total().amount(),
                expected,
                "{slices} slices priced differently"
            );
            assert_eq!(
                r.quantity_for(Dimension::Energy),
                dec("36"),
                "{slices} slices lost energy across the tiers"
            );
        }
    }

    #[test]
    fn a_time_of_day_boundary_inside_a_period_is_cut_too() {
        // A night rate that begins at 22:00 begins at 22:00, whatever periods
        // the caller hands over. Judged only at a period's start, one period
        // running 21:00 to 23:00 takes the day rate throughout — a quarter of
        // the session, decided by slicing.
        let t = tiered(vec![
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.30"))],
                restrictions: Restrictions {
                    start_time: Some(time::macros::time!(22:00)),
                    end_time: Some(time::macros::time!(06:00)),
                    ..Restrictions::default()
                },
            },
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.50"))]),
        ]);

        let evening =
            |minute: i64| datetime!(2026-01-02 21:00 +1) + time::Duration::minutes(minute);

        // 21:00 → 23:00, 20 kWh, as one period and as two. 10 × 0.50 + 10 × 0.30.
        let coarse =
            Chargeable::new(vec![Period::charging(evening(0), evening(120), kwh("20"))]).unwrap();
        let fine = Chargeable::new(vec![
            Period::charging(evening(0), evening(60), kwh("10")),
            Period::charging(evening(60), evening(120), kwh("10")),
        ])
        .unwrap();

        assert_eq!(rate(&t, &coarse).exact_total().amount(), dec("8.00"));
        assert_eq!(
            rate(&t, &coarse).exact_total(),
            rate(&t, &fine).exact_total(),
            "a price that depends on the granularity of the input is not a price"
        );
        // Two prices applied, so a tiered invoice has two lines to show.
        assert_eq!(rate(&t, &coarse).lines.len(), 2);
        assert_eq!(
            rate(&t, &coarse).quantity_for(Dimension::Energy),
            dec("20"),
            "the cut divides the session, it does not change it"
        );
    }

    #[test]
    fn a_clock_boundary_is_cut_on_every_day_a_period_spans() {
        // A 22:00 threshold is a threshold on every day there is. An overnight
        // session crosses 22:00 once and 06:00 once, on different dates, and
        // walking only the first day would leave the morning boundary uncut.
        let t = tiered(vec![
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.30"))],
                restrictions: Restrictions {
                    start_time: Some(time::macros::time!(22:00)),
                    end_time: Some(time::macros::time!(06:00)),
                    ..Restrictions::default()
                },
            },
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.50"))]),
        ]);

        // 21:00 Friday → 07:00 Saturday: one hour day, eight hours night, one
        // hour day. 1 kWh an hour, so 2 × 0.50 + 8 × 0.30 = 3.40.
        let start = datetime!(2026-01-02 21:00 +1);
        let s = Chargeable::new(vec![Period::charging(
            start,
            start + time::Duration::hours(10),
            kwh("10"),
        )])
        .unwrap();

        let r = rate(&t, &s);
        assert_eq!(r.exact_total().amount(), dec("3.40"));
        assert_eq!(r.quantity_for(Dimension::Energy), dec("10"));
    }

    #[test]
    fn a_date_restriction_is_cut_at_its_own_midnight() {
        // A price that starts on the third does not reach back into the second.
        let t = tiered(vec![
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.60"))],
                restrictions: Restrictions {
                    start_date: Some(time::macros::date!(2026 - 01 - 03)),
                    ..Restrictions::default()
                },
            },
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.40"))]),
        ]);

        // 23:00 → 01:00 across midnight, 2 kWh. One at 0.40, one at 0.60.
        let start = datetime!(2026-01-02 23:00 +1);
        let s = Chargeable::new(vec![Period::charging(
            start,
            start + time::Duration::hours(2),
            kwh("2"),
        )])
        .unwrap();

        let r = rate(&t, &s);
        assert_eq!(r.exact_total().amount(), dec("1.00"));
        assert_eq!(r.quantity_for(Dimension::Energy), dec("2"));
    }

    #[test]
    fn a_duration_threshold_inside_a_period_is_cut_too() {
        // "The first thirty minutes are free" has the same shape as an energy
        // tier and the same failure without the cut.
        let t = tiered(vec![
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Time, dec("0.00"))],
                restrictions: Restrictions {
                    max_duration_s: Some(1800),
                    ..Restrictions::default()
                },
            },
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Time, dec("6.00"))]),
        ]);

        // One hour as a single period: thirty free minutes, then thirty at
        // 6.00 an hour.
        let s = Chargeable::new(vec![Period::charging(at(0), at(60), kwh("20"))]).unwrap();
        let r = rate(&t, &s);
        assert_eq!(r.amount_for(Dimension::Time), Some(dec("3.00")));
        // Two lines, because the free half hour is a term of the price the
        // driver is entitled to see rather than a gap in it.
        assert_eq!(r.lines.len(), 2, "{:?}", r.lines);
        assert_eq!(r.lines[0].unit_price, dec("0.00"));
        assert_eq!(r.lines[0].quantity, dec("0.5"));
        assert_eq!(r.lines[1].unit_price, dec("6.00"));
        assert_eq!(r.lines[1].quantity, dec("0.5"));
        assert_eq!(
            r.quantity_for(Dimension::Time),
            dec("1.0"),
            "one whole hour"
        );
    }

    #[test]
    fn thresholds_landing_in_the_same_second_do_not_produce_negative_energy() {
        // An energy threshold and a duration threshold can land in the same
        // second and disagree about how much had been delivered by it. Emitting
        // both would give one piece a negative quantity; the sweep keeps the
        // first and the session still conserves.
        let t = tiered(vec![
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.39"))],
                restrictions: Restrictions {
                    max_kwh: Some(dec("5.001")),
                    max_duration_s: Some(450),
                    ..Restrictions::default()
                },
            },
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.59"))]),
        ]);
        let s = Chargeable::new(vec![Period::charging(at(0), at(15), kwh("10"))]).unwrap();
        let r = rate(&t, &s);

        assert_eq!(r.quantity_for(Dimension::Energy), dec("10"));
        assert!(
            r.lines.iter().all(|l| l.quantity >= Decimal::ZERO),
            "{:?}",
            r.lines
        );
    }

    #[test]
    fn a_period_no_threshold_touches_is_passed_through_untouched() {
        // Sub-second remainders survive, because rebuilding a whole period out
        // of whole seconds would drop them.
        let t = tiered(vec![
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.39"))],
                restrictions: Restrictions {
                    max_kwh: Some(dec("100")),
                    ..Restrictions::default()
                },
            },
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.59"))]),
        ]);
        let odd_end = at(15) + time::Duration::milliseconds(500);
        let s = Chargeable::new(vec![Period::charging(at(0), odd_end, kwh("10"))]).unwrap();

        let r = rate(&t, &s);
        assert_eq!(r.lines.len(), 1);
        assert_eq!(r.lines[0].unit_price, dec("0.39"));
        assert_eq!(r.quantity_for(Dimension::Energy), dec("10"));
    }

    #[test]
    fn cutting_a_period_conserves_its_energy_and_its_duration() {
        // Awkward on purpose: thresholds that do not land on a whole second,
        // and a total that does not divide.
        let t = tiered(vec![
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.39"))],
                restrictions: Restrictions {
                    max_kwh: Some(dec("3.7")),
                    ..Restrictions::default()
                },
            },
            TariffElement::unrestricted(vec![
                PriceComponent::new(Dimension::Energy, dec("0.59")),
                PriceComponent::new(Dimension::Time, dec("1.20")),
            ]),
        ]);
        let s = Chargeable::new(vec![Period::charging(at(0), at(21), kwh("13.37"))]).unwrap();
        let r = rate(&t, &s);

        assert_eq!(
            r.quantity_for(Dimension::Energy),
            dec("13.37"),
            "every kilowatt-hour is priced exactly once"
        );
        assert_eq!(
            r.lines
                .iter()
                .find(|l| l.dimension == Dimension::Energy && l.unit_price == dec("0.39"))
                .map(|l| l.quantity),
            Some(dec("3.7")),
            "the cut lands on the threshold, not near it"
        );
    }

    #[test]
    fn a_flat_fee_is_charged_once_however_many_periods_there_are() {
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::Flat, dec("0.50")),
        ]);
        let s = Chargeable::new(vec![
            Period::charging(at(0), at(15), kwh("5")),
            Period::charging(at(15), at(30), kwh("5")),
            Period::charging(at(30), at(45), kwh("5")),
        ])
        .unwrap();

        let r = rate(&t, &s);
        assert_eq!(r.amount_for(Dimension::Flat), Some(dec("0.50")));
        assert_eq!(r.quantity_for(Dimension::Flat), Decimal::ONE);
    }

    #[test]
    fn charging_time_and_occupancy_are_different_minutes() {
        // The distinction AFIR Art. 5(4) turns on: an occupancy fee is a price
        // for *not* charging, and folding them together charges twice for one
        // minute.
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::Time, dec("3.60")),
            PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
        ]);
        let s = Chargeable::new(vec![
            Period::charging(at(0), at(60), kwh("20")),
            Period::parked(at(60), at(90)),
        ])
        .unwrap();

        let r = rate(&t, &s);
        assert_eq!(r.amount_for(Dimension::Time), Some(dec("3.60")), "one hour");
        assert_eq!(
            r.amount_for(Dimension::ParkingTime),
            Some(dec("3.00")),
            "half an hour"
        );
        assert_eq!(s.charging_seconds(), 3600);
        assert_eq!(s.parking_seconds(), 1800);
    }

    #[test]
    fn every_term_of_the_total_is_a_line() {
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::Flat, dec("0.50")),
            PriceComponent::new(Dimension::Time, dec("0.06")),
        ]);
        let r = rate(&t, &session("10"));
        assert_eq!(r.lines.len(), 3);
        assert!(r.lines_sum_to_total());
        assert_eq!(r.lines_total(), r.exact_total().amount());
    }

    #[test]
    fn lines_come_out_in_the_order_afir_prescribes() {
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Flat, dec("0.50")),
            PriceComponent::new(Dimension::ParkingTime, dec("0.10")),
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::Time, dec("0.06")),
        ]);
        let s = Chargeable::new(vec![
            Period::charging(at(0), at(30), kwh("10")),
            Period::parked(at(30), at(40)),
        ])
        .unwrap();

        let r = rate(&t, &s);
        assert_eq!(
            r.lines.iter().map(|l| l.dimension).collect::<Vec<_>>(),
            vec![
                Dimension::Energy,
                Dimension::Time,
                Dimension::ParkingTime,
                Dimension::Flat
            ],
            "an invoice and a price display must list the same things the same way round"
        );
    }

    #[test]
    fn a_zero_quantity_produces_no_line_except_a_flat_fee() {
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::ParkingTime, dec("0.10")),
            PriceComponent::new(Dimension::Flat, dec("0.50")),
        ]);
        let r = rate(&t, &session("10"));
        assert_eq!(r.amount_for(Dimension::ParkingTime), None);
        assert_eq!(r.amount_for(Dimension::Flat), Some(dec("0.50")));
    }

    #[test]
    fn block_rounding_applies_once_to_the_session_not_once_per_period() {
        // Rounding every quarter hour up to a block would bill an eight-slot
        // session eight blocks it never used.
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.50")).with_step_size(1000),
        ]);
        let s = Chargeable::new(vec![
            Period::charging(at(0), at(15), kwh("3.4")),
            Period::charging(at(15), at(30), kwh("3.5")),
            Period::charging(at(30), at(45), kwh("3.5")),
        ])
        .unwrap();

        let r = rate(&t, &s);
        assert_eq!(r.lines[0].quantity, dec("11"), "10.4 kWh billed as 11");
        assert_eq!(r.exact_total().amount(), dec("5.50"));
        assert!(
            r.notes
                .iter()
                .any(|n| matches!(n, RatingNote::RoundedToBlock { .. })),
            "rounding up is always against the customer, so it is said out loud"
        );
    }

    #[test]
    fn a_step_size_of_one_rounds_nothing() {
        let t = ad_hoc(vec![PriceComponent::new(Dimension::Energy, dec("0.50"))]);
        let r = rate(&t, &session("10.4"));
        assert_eq!(r.lines[0].quantity, dec("10.4"));
        assert!(r.notes.is_empty());
    }

    #[test]
    fn a_minimum_moves_the_total_and_shows_its_own_term() {
        let mut t = ad_hoc(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        t.min_price = Some(PriceLimit::gross(dec("5.00")));
        let r = rate(&t, &session("1"));

        assert_eq!(r.total().amount(), dec("5.00"));
        assert!(!r.lines_sum_to_total(), "and the report admits it");
        let adjustment = r.adjustment.expect("a minimum was applied");
        assert_eq!(adjustment.kind, AdjustmentKind::Minimum);
        assert_eq!(adjustment.amount, dec("4.51"));
        assert_eq!(adjustment.lines_total, dec("0.49"));
    }

    #[test]
    fn a_minimum_on_a_session_with_no_lines_still_carries_its_tax() {
        // The case a minimum charge exists *for*: a driver plugged in, drew
        // nothing and left. There is no largest line to inherit a rate from, and
        // falling back to none put the whole charge in the zero-rate group —
        // €0.50 net, €0.00 tax, on a tariff that is 19 % throughout. An invoice
        // built from that under-declares its own VAT.
        let mut t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
        ]);
        t.min_price = Some(PriceLimit::gross(dec("0.50")));
        let r = rate(&t, &session("0"));

        assert!(r.lines.is_empty(), "nothing was delivered to charge for");
        assert_eq!(
            r.adjustment.expect("a minimum was applied").vat,
            Some(dec("19"))
        );

        let summary = r.tax_summary();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].rate, dec("19"));
        assert_eq!(summary[0].gross, dec("0.50"), "the driver still pays 0.50");
        assert_eq!(summary[0].net, dec("0.42"));
        assert_eq!(summary[0].tax, dec("0.08"));
        assert_eq!(r.gross().amount(), dec("0.50"));
    }

    #[test]
    fn a_minimum_under_rates_that_disagree_inherits_none_rather_than_guessing() {
        // The honest half of the same rule. Two components at two rates and no
        // line to choose between them is a question for the tariff's author.
        let mut t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
            PriceComponent::new(Dimension::ParkingTime, dec("6.00")).with_vat(dec("7")),
        ]);
        t.min_price = Some(PriceLimit::gross(dec("5.00")));
        // A charging period that delivered nothing: no energy to charge for and
        // no occupancy either, so neither component produces a line.
        let r = rate(&t, &session("0"));

        assert!(r.lines.is_empty(), "{:?}", r.lines);
        assert_eq!(r.adjustment.expect("a minimum was applied").vat, None);
    }

    #[test]
    fn a_vat_rate_with_no_split_is_reported_rather_than_a_panic() {
        // `net × (1 + rate/100)` is the gross, so at exactly −100 % the factor
        // is zero and no net grosses up to a non-zero amount. `Decimal` answers
        // a division by zero with a panic; a tariff from a roaming partner is
        // somebody else's document, so the rate is untrusted input and the
        // rating has to survive it.
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("-100")),
        ]);
        let r = rate(&t, &session("10"));

        assert!(
            r.reasons().any(|note| note.contains("no net and tax")),
            "{:?}",
            r.reasons().collect::<Vec<_>>()
        );
        // The price is still the price — only the split is unavailable.
        assert_eq!(r.total().amount(), dec("4.90"));
        let summary = r.tax_summary();
        assert_eq!(summary[0].gross, dec("4.90"));
        assert_eq!(summary[0].net, dec("4.90"), "reported whole");
        assert_eq!(summary[0].tax, Decimal::ZERO);
    }

    #[test]
    fn the_unsplittable_rate_is_only_unsplittable_on_a_gross_tariff() {
        // The factor is a *divisor* only when the prices are gross. On a net
        // tariff it is a multiplier, and zero multiplies fine — so the note
        // that says "reported whole" must not be attached to a breakdown that
        // was computed exactly. A partner reads these notes to decide whether a
        // taxable amount can be justified, and a false one is worse than none.
        let mut t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("-100")),
        ]);
        t.tax_included = TaxIncluded::No;
        let r = rate(&t, &session("10"));

        assert!(
            !r.reasons().any(|note| note.contains("no net and tax")),
            "{:?}",
            r.reasons().collect::<Vec<_>>()
        );
        let summary = r.tax_summary();
        assert_eq!(summary[0].net, dec("4.90"));
        assert_eq!(summary[0].gross, Decimal::ZERO, "net × 0 is defined");
        assert_eq!(summary[0].tax, dec("-4.90"));

        // …and a party outside a tax regime never reads the rate at all.
        t.tax_included = TaxIncluded::NotApplicable;
        assert!(
            !rate(&t, &session("10"))
                .reasons()
                .any(|note| note.contains("no net and tax"))
        );
    }

    #[test]
    fn a_maximum_caps_the_total_and_shows_its_own_term() {
        let mut t = ad_hoc(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        t.max_price = Some(PriceLimit::gross(dec("10.00")));
        let r = rate(&t, &session("100"));

        assert_eq!(r.total().amount(), dec("10.00"));
        let adjustment = r.adjustment.expect("a maximum was applied");
        assert_eq!(adjustment.kind, AdjustmentKind::Maximum);
        assert_eq!(adjustment.amount, dec("-39.00"));
    }

    #[test]
    fn gross_prices_have_their_tax_stripped_out() {
        // The German case: 19 % included, and the invoice has to show the
        // taxable amount separately [UStG §14].
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
        ]);
        let r = rate(&t, &session("29.500"));

        let summary = r.tax_summary();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].rate, dec("19"));
        assert_eq!(summary[0].gross, dec("14.46"));
        assert_eq!(summary[0].net, dec("12.15"));
        assert_eq!(summary[0].tax, dec("2.31"));
        assert_eq!(r.gross().to_string(), "14.46 EUR");
        assert_eq!((r.net().amount() + r.tax().amount()), r.gross().amount());
    }

    #[test]
    fn net_prices_have_the_tax_added_on() {
        let mut t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("1.00")).with_vat(dec("19")),
        ]);
        t.tax_included = TaxIncluded::No;
        let r = rate(&t, &session("100"));

        assert_eq!(
            r.total().to_string(),
            "100.00 EUR",
            "the stated basis is net"
        );
        assert_eq!(r.net().amount(), dec("100.00"));
        assert_eq!(r.tax().amount(), dec("19.00"));
        assert_eq!(r.gross().amount(), dec("119.00"));
    }

    #[test]
    fn two_vat_rates_produce_two_taxable_amounts() {
        // Lawful and not rare: delivered electricity and a service fee can sit
        // in different categories, and EN 16931 wants both.
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.50")).with_vat(dec("19")),
            PriceComponent::new(Dimension::Flat, dec("1.07")).with_vat(dec("7")),
        ]);
        let r = rate(&t, &session("100"));

        let summary = r.tax_summary();
        assert_eq!(summary.len(), 2);
        assert_eq!(summary[0].rate, dec("7"));
        assert_eq!(summary[0].gross, dec("1.07"));
        assert_eq!(summary[0].net, dec("1.00"));
        assert_eq!(summary[1].rate, dec("19"));
        assert_eq!(summary[1].gross, dec("50.00"));
        assert_eq!(r.gross().amount(), dec("51.07"));
    }

    #[test]
    fn an_untaxed_component_lands_in_the_zero_rate_category() {
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.50")).with_vat(dec("19")),
            PriceComponent::new(Dimension::Flat, dec("2.00")),
        ]);
        let r = rate(&t, &session("10"));
        let summary = r.tax_summary();
        assert_eq!(summary.len(), 2);
        assert_eq!(summary[0].rate, Decimal::ZERO);
        assert_eq!(summary[0].tax, Decimal::ZERO);
        assert_eq!(summary[0].net, dec("2.00"));
    }

    #[test]
    fn an_adjustment_is_taxed_at_the_rate_of_the_largest_line() {
        let mut t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
        ]);
        t.min_price = Some(PriceLimit::gross(dec("5.00")));
        let r = rate(&t, &session("1"));

        let summary = r.tax_summary();
        assert_eq!(summary.len(), 1, "the minimum joins the line it tops up");
        assert_eq!(summary[0].rate, dec("19"));
        assert_eq!(summary[0].gross, dec("5.00"));
        assert_eq!(r.adjustment.unwrap().vat, Some(dec("19")));
    }

    #[test]
    fn a_night_tariff_that_wraps_midnight_works() {
        // The classic bug: 22:00–06:00 read as an empty range.
        let night = TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.29"))],
            restrictions: Restrictions {
                start_time: Some(time::macros::time!(22:00)),
                end_time: Some(time::macros::time!(06:00)),
                ..Restrictions::default()
            },
        };
        let day =
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        let t = tiered(vec![night, day]);

        let at_23 = Chargeable::energy_only(
            kwh("10"),
            datetime!(2026-01-02 23:00 +1),
            datetime!(2026-01-02 23:30 +1),
        )
        .unwrap();
        assert_eq!(rate(&t, &at_23).lines[0].unit_price, dec("0.29"));

        let at_03 = Chargeable::energy_only(
            kwh("10"),
            datetime!(2026-01-02 03:00 +1),
            datetime!(2026-01-02 03:30 +1),
        )
        .unwrap();
        assert_eq!(rate(&t, &at_03).lines[0].unit_price, dec("0.29"));

        let at_noon = Chargeable::energy_only(
            kwh("10"),
            datetime!(2026-01-02 12:00 +1),
            datetime!(2026-01-02 12:30 +1),
        )
        .unwrap();
        assert_eq!(rate(&t, &at_noon).lines[0].unit_price, dec("0.49"));
    }

    #[test]
    fn a_session_that_crosses_into_the_night_rate_is_priced_on_both_sides() {
        // Which the whole-session reading cannot express at all.
        let night = TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.29"))],
            restrictions: Restrictions {
                start_time: Some(time::macros::time!(22:00)),
                end_time: Some(time::macros::time!(06:00)),
                ..Restrictions::default()
            },
        };
        let day =
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        let t = tiered(vec![night, day]);

        let s = Chargeable::new(vec![
            Period::charging(
                datetime!(2026-01-02 21:30 +1),
                datetime!(2026-01-02 22:00 +1),
                kwh("10"),
            ),
            Period::charging(
                datetime!(2026-01-02 22:00 +1),
                datetime!(2026-01-02 22:30 +1),
                kwh("10"),
            ),
        ])
        .unwrap();

        let r = rate(&t, &s);
        assert_eq!(r.lines.len(), 2);
        assert_eq!(r.amount_for(Dimension::Energy), Some(dec("7.80"))); // 4.90 + 2.90
    }

    #[test]
    fn a_power_restriction_selects_the_element() {
        let fast = TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.79"))],
            restrictions: Restrictions {
                min_power_kw: Some(dec("50")),
                ..Restrictions::default()
            },
        };
        let slow =
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        let t = tiered(vec![fast, slow]);

        // 30 kWh in a quarter hour is 120 kW.
        let quick = Chargeable::new(vec![Period::charging(at(0), at(15), kwh("30"))]).unwrap();
        assert_eq!(rate(&t, &quick).lines[0].unit_price, dec("0.79"));

        // 3 kWh in a quarter hour is 12 kW.
        let gentle = Chargeable::new(vec![Period::charging(at(0), at(15), kwh("3"))]).unwrap();
        assert_eq!(rate(&t, &gentle).lines[0].unit_price, dec("0.49"));
    }

    #[test]
    fn a_date_window_selects_the_element() {
        let winter = TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.59"))],
            restrictions: Restrictions {
                start_date: Some(time::macros::date!(2026 - 01 - 01)),
                end_date: Some(time::macros::date!(2026 - 03 - 01)),
                ..Restrictions::default()
            },
        };
        let rest =
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        let t = tiered(vec![winter, rest]);

        assert_eq!(rate(&t, &session("10")).lines[0].unit_price, dec("0.59"));

        let in_march = Chargeable::energy_only(
            kwh("10"),
            datetime!(2026-03-02 10:00 +1),
            datetime!(2026-03-02 10:30 +1),
        )
        .unwrap();
        assert_eq!(rate(&t, &in_march).lines[0].unit_price, dec("0.49"));
    }

    #[test]
    fn an_element_with_an_unevaluable_restriction_is_skipped_not_assumed_open() {
        // A partner sends a restriction this build has never heard of. Treating
        // it as absent applies the price under conditions nobody checked.
        let unknown = TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.19"))],
            restrictions: Restrictions {
                unevaluable: vec!["reservation=RESERVATION_EXPIRES".to_owned()],
                ..Restrictions::default()
            },
        };
        let ordinary =
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        let t = tiered(vec![unknown, ordinary]);

        let r = rate(&t, &session("10"));
        assert_eq!(r.lines[0].unit_price, dec("0.49"));
        assert!(
            r.notes
                .iter()
                .any(|n| matches!(n, RatingNote::UnevaluableRestriction { .. })),
            "{:?}",
            r.notes
        );
        assert!(r.reasons().any(|s| s.contains("reservation")));
    }

    #[test]
    fn a_weekday_restriction_is_honoured() {
        let weekend = TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.29"))],
            restrictions: Restrictions {
                days_of_week: vec![time::Weekday::Saturday, time::Weekday::Sunday],
                ..Restrictions::default()
            },
        };
        let t = tiered(vec![
            weekend,
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]),
        ]);

        // 2026-01-02 is a Friday; 2026-01-03 a Saturday.
        assert_eq!(rate(&t, &session("10")).lines[0].unit_price, dec("0.49"));
        let saturday = Chargeable::energy_only(
            kwh("10"),
            datetime!(2026-01-03 10:00 +1),
            datetime!(2026-01-03 10:30 +1),
        )
        .unwrap();
        assert_eq!(rate(&t, &saturday).lines[0].unit_price, dec("0.29"));
    }

    #[test]
    fn a_session_crossing_midnight_is_cut_at_the_weekday_boundary() {
        // A weekday changes at local midnight and names no date to cut at, so
        // a period that spans one would otherwise be priced end to end at the
        // day it started in.
        // Friday 23:00 to Saturday 01:00 is one hour of each.
        let weekend = TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.29"))],
            restrictions: Restrictions {
                days_of_week: vec![time::Weekday::Saturday, time::Weekday::Sunday],
                ..Restrictions::default()
            },
        };
        let t = tiered(vec![
            weekend,
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]),
        ]);

        // 2026-01-02 is a Friday. One period, two hours, twenty kilowatt-hours.
        let overnight = Chargeable::energy_only(
            kwh("20"),
            datetime!(2026-01-02 23:00 +1),
            datetime!(2026-01-03 01:00 +1),
        )
        .unwrap();

        let r = rate(&t, &overnight);
        assert_eq!(
            r.lines.len(),
            2,
            "one line per side of midnight: {:?}",
            r.lines
        );
        assert_eq!(r.lines[0].unit_price, dec("0.49"), "the Friday hour");
        assert_eq!(r.lines[0].quantity, dec("10"));
        assert_eq!(r.lines[1].unit_price, dec("0.29"), "the Saturday hour");
        assert_eq!(r.lines[1].quantity, dec("10"));
        // 10 × 0.49 + 10 × 0.29
        assert_eq!(r.exact_total().amount(), dec("7.80"));
        assert_eq!(
            r.quantity_for(Dimension::Energy),
            dec("20"),
            "and the cut conserves, like every other one"
        );

        // …and the answer does not depend on how the caller sliced it.
        let sliced = Chargeable::new(
            (0..8)
                .map(|i| {
                    Period::charging(
                        datetime!(2026-01-02 23:00 +1) + time::Duration::minutes(15 * i),
                        datetime!(2026-01-02 23:00 +1) + time::Duration::minutes(15 * (i + 1)),
                        kwh("2.5"),
                    )
                })
                .collect(),
        )
        .unwrap();
        assert_eq!(rate(&t, &sliced).exact_total().amount(), dec("7.80"));
    }

    #[test]
    fn a_period_nothing_matches_is_named_rather_than_swallowed() {
        let t = tiered(vec![TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.49"))],
            restrictions: Restrictions {
                min_kwh: Some(dec("100")),
                ..Restrictions::default()
            },
        }]);
        let r = rate(&t, &session("10"));
        assert!(r.lines.is_empty());
        assert_eq!(r.total().amount(), Decimal::ZERO);
        // The quantity, not just the fact: a session that delivered ten
        // kilowatt-hours and priced none of them is a dispute, and this is the
        // number it is about.
        assert_eq!(
            r.notes,
            vec![RatingNote::Unpriced {
                dimension: Dimension::Energy,
                at: at(0),
                periods: 1,
                base_quantity: dec("10"),
            }]
        );
        assert!(r.reasons().any(|s| s.contains("10 kWh was not charged")));
    }

    #[test]
    fn one_dimension_charged_at_two_vat_rates_is_two_taxable_amounts() {
        // A tiered tariff whose tiers sit in different tax categories. Reading
        // one rate off the first line and applying it to the summed amount —
        // the shape a per-dimension cost on the wire invites — taxes the second
        // tier at the first tier's rate.
        let t = tiered(vec![
            TariffElement {
                components: vec![
                    PriceComponent::new(Dimension::Energy, dec("1.19")).with_vat(dec("19")),
                ],
                restrictions: Restrictions {
                    max_kwh: Some(dec("10")),
                    ..Restrictions::default()
                },
            },
            TariffElement::unrestricted(vec![
                PriceComponent::new(Dimension::Energy, dec("1.07")).with_vat(dec("7")),
            ]),
        ]);

        let r = rate(&t, &session("20"));
        let energy = r.tax_summary_for(Dimension::Energy);
        assert_eq!(energy.len(), 2, "two categories: {energy:?}");
        assert_eq!(energy[0].rate, dec("7"));
        assert_eq!(energy[0].net, dec("10.00")); // 10 × 1.07 gross
        assert_eq!(energy[0].tax, dec("0.70"));
        assert_eq!(energy[1].rate, dec("19"));
        assert_eq!(energy[1].net, dec("10.00")); // 10 × 1.19 gross
        assert_eq!(energy[1].tax, dec("1.90"));

        // …and it agrees with the whole-record summary, which has no other
        // dimension to add.
        assert_eq!(energy, r.tax_summary());
    }

    #[test]
    fn a_dimensions_breakdown_leaves_the_adjustment_to_the_total() {
        // A minimum charge is a term of the total, not of any one heading.
        // Attributing it to one would make the headings sum to more than the
        // record's own lines.
        let mut t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
        ]);
        t.min_price = Some(PriceLimit::gross(dec("10.00")));

        let r = rate(&t, &session("1"));
        assert!(r.adjustment.is_some());
        assert_eq!(
            r.tax_summary_for(Dimension::Energy)
                .iter()
                .map(|l| l.gross)
                .sum::<Decimal>(),
            dec("0.49"),
            "the heading is the line, not the topped-up total"
        );
        assert_eq!(r.gross().amount(), dec("10.00"));
    }

    #[test]
    fn one_note_per_dimension_however_many_periods_went_unpriced() {
        // Ninety-six identical notes are the same fact reported ninety-six
        // times, and a record that carries them is one nobody reads.
        let t = tiered(vec![TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.49"))],
            restrictions: Restrictions {
                min_kwh: Some(dec("100")),
                ..Restrictions::default()
            },
        }]);
        let s = Chargeable::new(vec![
            Period::charging(at(0), at(15), kwh("4")),
            Period::charging(at(15), at(30), kwh("3")),
            Period::charging(at(30), at(45), kwh("2.5")),
        ])
        .unwrap();

        assert_eq!(
            rate(&t, &s).notes,
            vec![RatingNote::Unpriced {
                dimension: Dimension::Energy,
                at: at(0),
                periods: 3,
                base_quantity: dec("9.5"),
            }]
        );
    }

    #[test]
    fn a_price_component_is_chosen_per_dimension_not_per_element() {
        // The shape `[OCPI 2.3.0 §Tariff]` recommends and every partner sends:
        // one unrestricted default element per dimension. Read as "the first
        // element that matches, then all of its components", this bills the
        // session fee and silently drops every kilowatt-hour — no element
        // failed to match, so nothing is even reported.
        let t = tiered(vec![
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Flat, dec("0.50"))]),
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]),
        ]);

        let r = rate(&t, &session("10"));
        assert_eq!(r.lines.len(), 2, "{:?}", r.lines);
        assert_eq!(r.amount_for(Dimension::Energy), Some(dec("4.90")));
        assert_eq!(r.amount_for(Dimension::Flat), Some(dec("0.50")));
        assert_eq!(r.exact_total().amount(), dec("5.40"));
        assert!(r.notes.is_empty(), "nothing was assumed: {:?}", r.notes);
    }

    #[test]
    fn a_restricted_element_shadows_only_the_dimension_it_prices() {
        // A night rate on energy, a session fee that applies always, and a day
        // rate behind both. The flat element sits *between* the two energy
        // elements, which under a per-element reading would end the search.
        let t = tiered(vec![
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.29"))],
                restrictions: Restrictions {
                    start_time: Some(time::macros::time!(22:00)),
                    end_time: Some(time::macros::time!(6:00)),
                    ..Restrictions::default()
                },
            },
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Flat, dec("0.50"))]),
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]),
        ]);

        // 10:00 is the day rate, and the fee applies beside it.
        let day = rate(&t, &session("10"));
        assert_eq!(day.amount_for(Dimension::Energy), Some(dec("4.90")));
        assert_eq!(day.amount_for(Dimension::Flat), Some(dec("0.50")));

        // …and at 23:00 the same tariff takes the night rate, still with the fee.
        let night = Chargeable::energy_only(
            kwh("10"),
            datetime!(2026-01-02 23:00 +1),
            datetime!(2026-01-02 23:30 +1),
        )
        .unwrap();
        let night = rate(&t, &night);
        assert_eq!(night.amount_for(Dimension::Energy), Some(dec("2.90")));
        assert_eq!(night.amount_for(Dimension::Flat), Some(dec("0.50")));
    }

    #[test]
    fn one_dimension_can_go_unpriced_while_another_is_charged() {
        // `[OCPI 2.3.0 §Tariff]`: "there will be no costs for that Tariff
        // Dimension" — a price of zero rather than an error, and a fact the
        // record carries.
        let t = tiered(vec![
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]),
            TariffElement {
                components: vec![PriceComponent::new(Dimension::ParkingTime, dec("6.00"))],
                restrictions: Restrictions {
                    min_duration_s: Some(3600),
                    ..Restrictions::default()
                },
            },
        ]);
        let s = Chargeable::new(vec![
            Period::charging(at(0), at(15), kwh("10")),
            Period::parked(at(15), at(30)),
        ])
        .unwrap();

        let r = rate(&t, &s);
        assert_eq!(r.amount_for(Dimension::Energy), Some(dec("4.90")));
        assert_eq!(r.amount_for(Dimension::ParkingTime), None);
        assert_eq!(
            r.notes,
            vec![RatingNote::Unpriced {
                dimension: Dimension::ParkingTime,
                at: at(15),
                periods: 1,
                base_quantity: dec("900"),
            }],
            "fifteen minutes of occupancy nothing priced, in seconds"
        );
    }

    #[test]
    fn a_flat_fee_is_one_fee_whether_it_is_charged_or_missed() {
        // Charged late: the periods that missed it were overtaken, not lost.
        let late = tiered(vec![TariffElement {
            components: vec![PriceComponent::new(Dimension::Flat, dec("0.50"))],
            restrictions: Restrictions {
                min_duration_s: Some(900),
                ..Restrictions::default()
            },
        }]);
        let s = Chargeable::new(vec![
            Period::charging(at(0), at(15), kwh("5")),
            Period::charging(at(15), at(30), kwh("5")),
        ])
        .unwrap();
        let r = rate(&late, &s);
        assert_eq!(r.amount_for(Dimension::Flat), Some(dec("0.50")));
        assert!(r.notes.is_empty(), "{:?}", r.notes);

        // Never charged: one note, one session, not one per period.
        let never = tiered(vec![TariffElement {
            components: vec![PriceComponent::new(Dimension::Flat, dec("0.50"))],
            restrictions: Restrictions {
                min_duration_s: Some(86_400),
                ..Restrictions::default()
            },
        }]);
        assert_eq!(
            rate(&never, &s).notes,
            vec![RatingNote::Unpriced {
                dimension: Dimension::Flat,
                at: at(0),
                periods: 2,
                base_quantity: Decimal::ONE,
            }]
        );
    }

    #[test]
    fn an_element_pricing_the_same_dimension_twice_charges_it_once() {
        // Malformed — `[OCPI 2.3.0 §Tariff]` allows one active component per
        // dimension — and charging both would double-bill the energy.
        let t = tiered(vec![TariffElement::unrestricted(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::Energy, dec("0.59")),
        ])]);
        let r = rate(&t, &session("10"));
        assert_eq!(r.lines.len(), 1);
        assert_eq!(r.lines[0].unit_price, dec("0.49"), "the first one wins");
    }

    #[test]
    fn overlapping_periods_are_refused_before_anything_is_billed_twice() {
        let err = Chargeable::new(vec![
            Period::charging(at(0), at(30), kwh("10")),
            Period::charging(at(15), at(45), kwh("10")),
        ])
        .unwrap_err();
        assert!(matches!(err, ChargeableError::Overlap { .. }));
        assert!(err.to_string().contains("charged twice"));

        assert!(matches!(
            Chargeable::new(vec![]),
            Err(ChargeableError::Empty)
        ));
        assert!(matches!(
            Chargeable::energy_only(kwh("1"), at(30), at(0)),
            Err(ChargeableError::EndsBeforeItStarts { .. })
        ));
    }

    #[test]
    fn periods_are_sorted_so_the_cumulative_state_is_the_real_one() {
        let s = Chargeable::new(vec![
            Period::charging(at(15), at(30), kwh("5")),
            Period::charging(at(0), at(15), kwh("10")),
        ])
        .unwrap();
        assert_eq!(s.started_at(), at(0));
        assert_eq!(s.ended_at(), at(30));
        assert_eq!(s.total_energy(), kwh("15"));
    }

    /// A night tariff, in the zone its wall clock is written in.
    fn night_tariff(zone: &str) -> Tariff {
        Tariff {
            id: "n".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: TimeZone::new(zone).unwrap(),
            tax_included: TaxIncluded::Yes,
            elements: vec![
                TariffElement {
                    components: vec![PriceComponent::new(Dimension::Energy, dec("0.30"))],
                    restrictions: Restrictions {
                        start_time: Some(time::macros::time!(22:00)),
                        end_time: Some(time::macros::time!(06:00)),
                        ..Restrictions::default()
                    },
                },
                TariffElement::unrestricted(vec![PriceComponent::new(
                    Dimension::Energy,
                    dec("0.50"),
                )]),
            ],
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        }
    }

    #[test]
    fn one_instant_is_one_price_however_its_clock_was_spelled() {
        // The whole reason a tariff carries a zone. These are the *same two
        // hours* — 22:00 to midnight in Berlin — written three ways. Judged
        // against the offset each timestamp happens to carry, the UTC spelling
        // prices the first hour at the day rate and the session costs fifty per
        // cent more; judged in the tariff's zone, all three agree.
        let night = night_tariff("Europe/Berlin");
        let spellings = [
            (
                datetime!(2026-01-02 21:00 +0),
                datetime!(2026-01-02 23:00 +0),
            ),
            (
                datetime!(2026-01-02 22:00 +1),
                datetime!(2026-01-03 00:00 +1),
            ),
            (
                datetime!(2026-01-03 06:00 +9),
                datetime!(2026-01-03 08:00 +9),
            ),
        ];
        for (from, to) in spellings {
            let session = Chargeable::energy_only(kwh("20"), from, to).unwrap();
            let rated = rate(&night, &session);
            assert_eq!(
                rated.total().to_string(),
                "6.00 EUR",
                "20 kWh entirely inside the Berlin night window, stamped {from}"
            );
            assert_eq!(
                rated.lines.len(),
                1,
                "one price, not two: {:?}",
                rated.lines
            );
        }
    }

    #[test]
    fn the_zone_and_not_the_timestamp_decides_where_the_night_begins() {
        // One session, one set of instants, two zones. Lisbon is an hour behind
        // Berlin, so 21:00–23:00 UTC is 22:00–00:00 in Berlin — all night — and
        // 21:00–23:00 in Lisbon, of which only the last hour is.
        let session = Chargeable::energy_only(
            kwh("20"),
            datetime!(2026-01-02 21:00 +0),
            datetime!(2026-01-02 23:00 +0),
        )
        .unwrap();
        assert_eq!(
            rate(&night_tariff("Europe/Berlin"), &session)
                .total()
                .to_string(),
            "6.00 EUR"
        );
        assert_eq!(
            rate(&night_tariff("Europe/Lisbon"), &session)
                .total()
                .to_string(),
            // Ten kWh at the day rate, ten at the night rate.
            "8.00 EUR"
        );
    }

    #[test]
    fn the_answer_does_not_depend_on_how_the_session_was_sliced_across_a_zone() {
        // The same property the threshold cuts exist for, now that the cuts are
        // placed in the zone: one session, seven granularities, one total.
        let night = night_tariff("Europe/Berlin");
        let from = datetime!(2026-01-02 21:00 +0);
        let mut totals = Vec::new();
        for slices in [1_i64, 2, 3, 4, 6, 8, 12] {
            let minutes = 120 / slices;
            let periods: Vec<Period> = (0..slices)
                .map(|i| {
                    Period::charging(
                        from + time::Duration::minutes(i * minutes),
                        from + time::Duration::minutes((i + 1) * minutes),
                        kwh(&(Decimal::from(20) / Decimal::from(slices)).to_string()),
                    )
                })
                .collect();
            totals.push(rate(&night, &Chargeable::new(periods).unwrap()).total());
        }
        assert!(
            totals.windows(2).all(|w| w[0] == w[1]),
            "one session priced seven ways: {totals:?}"
        );
    }

    #[test]
    fn a_clock_change_moves_the_boundary_rather_than_the_price() {
        // Europe/Berlin springs forward at 02:00 local on 2026-03-29, so the
        // 06:00 end of the night window falls an hour earlier in UTC than it
        // does on any other day: 04:00 rather than 05:00. Six hours of a
        // constant 10 kWh/h therefore split four/two across the change, and
        // five/one the day before — the same tariff, the same session length,
        // a different answer, and the zone is the only thing that knows.
        let night = night_tariff("Europe/Berlin");
        let six_hours_from = |from: time::OffsetDateTime| {
            rate(
                &night,
                &Chargeable::energy_only(kwh("60"), from, from + time::Duration::hours(6)).unwrap(),
            )
        };

        // 40 kWh at 0.30 and 20 at 0.50.
        let across = six_hours_from(datetime!(2026-03-29 00:00 +0));
        assert_eq!(across.total().to_string(), "22.00 EUR");
        assert_eq!(across.lines.len(), 2, "{:?}", across.lines);

        // The ordinary day before: 50 kWh at 0.30 and 10 at 0.50.
        let ordinary = six_hours_from(datetime!(2026-03-28 00:00 +0));
        assert_eq!(ordinary.total().to_string(), "20.00 EUR");
    }

    #[test]
    fn the_hour_that_happens_twice_is_cut_twice() {
        // Europe/Berlin falls back at 03:00 local on 2026-10-25, so the wall
        // clock passes 02:30 twice. A tariff whose night window *ends* at 02:30
        // therefore ends twice: the day rate applies from the first crossing,
        // and the second crossing is inside the repeated hour.
        let mut tariff = night_tariff("Europe/Berlin");
        tariff.elements[0].restrictions.end_time = Some(time::macros::time!(02:30));
        // 00:00 UTC (02:00 local, summer time) to 02:00 UTC (03:00 local).
        // Night until 00:30 UTC, day from there.
        let session = Chargeable::energy_only(
            kwh("20"),
            datetime!(2026-10-25 00:00 +0),
            datetime!(2026-10-25 02:00 +0),
        )
        .unwrap();
        let rated = rate(&tariff, &session);
        let cuts: Vec<Decimal> = rated.lines.iter().map(|l| l.unit_price).collect();
        assert!(
            cuts.contains(&dec("0.30")) && cuts.contains(&dec("0.50")),
            "the repeated hour has to be cut at both crossings: {:?}",
            rated.lines
        );
        // 5 kWh night, 15 kWh day.
        assert_eq!(rated.total().to_string(), "9.00 EUR");
    }

    #[test]
    fn a_line_reproduces_its_own_amount_from_its_own_numbers() {
        // Twenty-five minutes at 6.00 an hour is 2.50 exactly — and 25/60 of an
        // hour is 0.41666… to the decimal's last digit, so a line whose amount
        // came from *that* figure would be 2.5000000000000000000000000002 and
        // would not reconcile against anything. The base quantity is the whole
        // seconds, and the amount is computed from it.
        let tariff = Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            TimeZone::new("Europe/Berlin").unwrap(),
            vec![PriceComponent::new(Dimension::ParkingTime, dec("6.00"))],
        );
        let session = Chargeable::new(vec![Period::parked(at(0), at(25))]).unwrap();
        let rated = rate(&tariff, &session);

        let line = &rated.lines[0];
        assert_eq!(line.base_quantity, dec("1500"), "whole seconds");
        assert_eq!(line.amount, dec("2.50"));
        assert!(line.reconciles());
        assert_ne!(
            line.quantity * line.unit_price,
            line.amount,
            "the hours figure genuinely cannot reproduce it, which is why there are two"
        );
    }

    #[test]
    fn every_line_of_every_shape_of_tariff_reconciles() {
        // The invariant a receiving party checks before it disputes a total,
        // over the shapes that reach it: energy, charging time, occupancy, a
        // session fee, a block rounding and a tier.
        let tariff = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::Contract,
            time_zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            tax_included: TaxIncluded::Yes,
            elements: vec![
                TariffElement {
                    components: vec![
                        PriceComponent::new(Dimension::Energy, dec("0.39")),
                        PriceComponent::new(Dimension::Time, dec("2.50")),
                        PriceComponent::new(Dimension::ParkingTime, dec("6.00"))
                            .with_step_size(900),
                        PriceComponent::new(Dimension::Flat, dec("0.35")),
                    ],
                    restrictions: Restrictions {
                        max_kwh: Some(dec("10")),
                        ..Restrictions::default()
                    },
                },
                TariffElement::unrestricted(vec![PriceComponent::new(
                    Dimension::Energy,
                    dec("0.59"),
                )]),
            ],
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        };
        let session = Chargeable::new(vec![
            Period::charging(at(0), at(22), kwh("15")),
            Period::parked(at(22), at(53)),
        ])
        .unwrap();

        let rated = rate(&tariff, &session);
        assert!(rated.lines.len() >= 4, "{:?}", rated.lines);
        assert!(rated.lines_reconcile(), "{:?}", rated.lines);
        assert_eq!(
            rated.base_quantity_for(Dimension::Energy),
            dec("15"),
            "the tiers divide the energy, they do not change it"
        );
    }

    #[test]
    fn a_slice_with_no_duration_cannot_meet_a_power_restriction_and_says_which_energy_it_left() {
        // The two features of this engine meet here and one of them wins.
        //
        // An energy threshold inside a period too short to hold it is cut on the
        // register alone, and the slice that opens is degenerate in time (D221).
        // A power restriction is `energy / duration`, which such a slice has no
        // answer for — so an element carrying one cannot price it, whatever its
        // other restrictions say.
        //
        // That is the right way round. The alternative is what this engine used
        // to do: drop the cut for want of a second and price the *whole* period
        // in the tier it began in, silently, at a total that depends on how
        // finely the input was sliced. Here the kilowatt-hour is named — one
        // `Unpriced` note with the quantity and the instant — beside the
        // `PowerJudgedPerPeriod` note that already says a power restriction makes
        // the total a function of the resolution. A quantity nobody priced is a
        // line somebody has to answer for; a quantity priced in the wrong tier
        // is not.
        let tariff = tiered(vec![
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.30"))],
                restrictions: Restrictions {
                    max_kwh: Some(dec("1")),
                    min_power_kw: Some(dec("1")),
                    ..Restrictions::default()
                },
            },
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.50"))],
                restrictions: Restrictions {
                    min_kwh: Some(dec("1")),
                    ..Restrictions::default()
                },
            },
        ]);

        // One second carrying two kilowatt-hours: the 1 kWh threshold falls
        // inside it and there is no interior second to put the cut on.
        let session = Chargeable::new(vec![Period::charging(
            at(0),
            at(0) + time::Duration::seconds(1),
            kwh("2"),
        )])
        .unwrap();
        let rated = rate(&tariff, &session);

        assert_eq!(rated.lines.len(), 1, "{:?}", rated.lines);
        assert_eq!(rated.lines[0].unit_price, dec("0.50"), "the second tier");
        assert_eq!(rated.lines[0].base_quantity, dec("1"));
        assert_eq!(rated.total().to_string(), "0.50 EUR");

        // …and the kilowatt-hour the first tier could not price is on the record
        // rather than folded into the one that could.
        assert!(
            rated.notes.iter().any(|note| matches!(
                note,
                RatingNote::Unpriced {
                    dimension: Dimension::Energy,
                    periods: 1,
                    ..
                }
            )),
            "{:?}",
            rated.notes
        );
        assert!(
            rated
                .notes
                .iter()
                .any(|note| matches!(note, RatingNote::PowerJudgedPerPeriod { index: 0, .. })),
            "{:?}",
            rated.notes
        );
    }

    #[test]
    fn a_power_restriction_makes_the_total_depend_on_the_slicing_and_says_so() {
        // The one restriction a period carries no information to cut on.
        // Average power is `energy / duration` over whichever window is asked
        // about, so one physical hour measured two ways gives two readings —
        // and the coarse one does not contain the fine one. Neither figure is
        // an arithmetic error; the finer is the better measurement, and the
        // note is what says the total is a function of the resolution.
        let mut t = ad_hoc(vec![PriceComponent::new(Dimension::Energy, dec("0.60"))]);
        t.elements.insert(
            0,
            TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.30"))],
                restrictions: Restrictions {
                    max_power_kw: Some(dec("50")),
                    ..Restrictions::default()
                },
            },
        );

        // 60 kWh in an hour: 60 kW average, so the whole hour is above the bound.
        let one = Chargeable::new(vec![Period::charging(at(0), at(60), kwh("60"))]).unwrap();
        // The same hour and the same energy, as two halves averaging 110 and 10.
        let two = Chargeable::new(vec![
            Period::charging(at(0), at(30), kwh("55")),
            Period::charging(at(30), at(60), kwh("5")),
        ])
        .unwrap();

        let coarse = rate(&t, &one);
        let fine = rate(&t, &two);
        assert_eq!(coarse.total().to_string(), "36.00 EUR");
        assert_eq!(fine.total().to_string(), "34.50 EUR");

        // Which is the point: both are priced, and both say the figure is not a
        // function of the session alone.
        for rated in [&coarse, &fine] {
            assert!(
                rated.notes.iter().any(|n| matches!(
                    n,
                    RatingNote::PowerJudgedPerPeriod {
                        index: 0,
                        max_kw: Some(_),
                        ..
                    }
                )),
                "{:?}",
                rated.notes
            );
        }
        assert!(
            coarse
                .reasons()
                .any(|r| r.contains("rate the periods the meter produced"))
        );

        // A tariff with no power restriction says nothing, because there is
        // nothing to say.
        assert!(
            rate(
                &ad_hoc(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]),
                &one
            )
            .notes
            .is_empty()
        );
    }
}
