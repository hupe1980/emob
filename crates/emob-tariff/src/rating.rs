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
//! # The wall clock is read in the offset the period carries
//!
//! "0.30 from 22:00" is a statement about local civil time, and a
//! `time::OffsetDateTime` carries a **UTC offset, not a time zone** — so the
//! only frame this crate can judge it in is the one each period states. That is
//! exact for every session `emob-session` assembles, because the quarter-hour
//! split gives every period one offset, and it is exact across a clock change
//! too as long as the periods either side carry the offsets their readings did.
//!
//! A session assembled from somebody else's timestamps can carry two, and then
//! the cuts are all placed in the first period's frame. [`rate`] says so —
//! [`RatingNote::MixedUtcOffsets`] — rather than letting an hour of night rate
//! land on the wrong side of a boundary in silence. Nothing here consults a
//! time-zone database: that would make a replayed rating depend on which
//! version of `tzdata` was installed, which is the one thing a dispute two
//! years old cannot afford.
//!
//! # Rounding happens once, at the end
//!
//! Each line is computed exactly and kept exact. Only [`Rated::total`] and the
//! tax breakdown round, to the currency's minor unit, half away from zero.
//! Rounding per line and then summing gives a different answer, and which of
//! the two is correct is a tax question rather than an arithmetic one — so the
//! exact figures survive and the caller can do either.

use emob_core::Energy;
use emob_core::quantity::{Currency, Money};
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
/// `charging` is the distinction `[AFIR Art. 5(4)]` turns on: an occupancy fee
/// is a price for *not* charging, and folding the two together is how a fast
/// charger ends up charging twice for the same minute.
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
    /// Whether energy was flowing. A period at zero energy that is still
    /// marked `charging` is a taper, not an occupancy.
    pub charging: bool,
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
            charging: true,
        }
    }

    /// A period in which the vehicle was connected but not charging.
    #[must_use]
    pub const fn parked(start: time::OffsetDateTime, end: time::OffsetDateTime) -> Self {
        Self {
            start,
            end,
            energy: Energy::ZERO,
            charging: false,
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

/// What a session did, period by period.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Chargeable {
    periods: Vec<Period>,
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

        Ok(Self { periods })
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
        self.periods
            .iter()
            .filter(|p| p.charging)
            .map(Period::seconds)
            .sum()
    }

    /// Seconds spent connected but not charging.
    #[must_use]
    pub fn parking_seconds(&self) -> u64 {
        self.periods
            .iter()
            .filter(|p| !p.charging)
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionState {
    /// Energy delivered before this period.
    pub energy_kwh: Decimal,
    /// Seconds elapsed before this period.
    pub elapsed_seconds: u64,
    /// When the period begins.
    pub at: time::OffsetDateTime,
    /// The average power across this period, if it has a duration.
    pub power_kw: Option<Decimal>,
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
    /// A quantity was rounded up to the component's block size.
    RoundedToBlock {
        /// Which dimension.
        dimension: Dimension,
        /// What was actually used.
        actual: Decimal,
        /// What was billed.
        billed: Decimal,
    },
    /// The total was moved by a minimum or maximum.
    Adjusted(Adjustment),
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
    /// The session's periods carry more than one UTC offset, and a wall-clock
    /// restriction was judged in one of them.
    ///
    /// A `time::OffsetDateTime` carries an offset, not a time zone, so "0.30
    /// from 22:00" can only be read against the offset the period itself
    /// states. Where every period agrees — which is what the quarter-hour split
    /// produces — that is exactly right. Where they do not, the session crossed
    /// a clock change or was assembled from timestamps in two frames, and every
    /// cut was placed in the first period's, so a boundary on the far side of
    /// the change is an hour out.
    ///
    /// Not an error: the energy, the durations and the totals are unaffected,
    /// and no session this workspace assembles carries mixed offsets. It is
    /// reported because a session built from a partner's document can, and an
    /// hour of night rate is worth more than the silence.
    MixedUtcOffsets {
        /// The offset the wall clock was judged in, in seconds east of UTC.
        judged_in_seconds: i32,
        /// An offset some other period carried.
        also_seen_seconds: i32,
    },
}

impl core::fmt::Display for RatingNote {
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
            Self::UnevaluableRestriction {
                index,
                restrictions,
            } => write!(
                f,
                "element {index} carries restrictions this build cannot evaluate and was skipped: {restrictions:?}"
            ),
            Self::RoundedToBlock {
                dimension,
                actual,
                billed,
            } => write!(
                f,
                "{dimension:?} rounded up from {actual} to {billed} {}",
                dimension.unit()
            ),
            Self::VatRateNotUsable { dimension, rate } => write!(
                f,
                "{dimension:?} carries a VAT rate of {rate} %, from which no net and tax can be computed: the amount is reported whole"
            ),
            Self::MixedUtcOffsets {
                judged_in_seconds,
                also_seen_seconds,
            } => write!(
                f,
                "this session's periods carry more than one UTC offset ({judged_in_seconds} s and {also_seen_seconds} s); every time-of-day restriction was judged in the first period's, so a boundary on the far side of the change is placed an hour out"
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
            self.lines
                .iter()
                .map(|line| (line.vat, line.amount))
                .chain(self.adjustment.map(|a| (a.vat, a.amount))),
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

    /// The taxable amount across every category.
    #[must_use]
    pub fn net(&self) -> Money {
        Money::new(
            self.tax_summary().iter().map(|t| t.net).sum(),
            self.currency,
        )
    }

    /// The tax across every category.
    #[must_use]
    pub fn tax(&self) -> Money {
        Money::new(
            self.tax_summary().iter().map(|t| t.tax).sum(),
            self.currency,
        )
    }

    /// What the driver pays.
    #[must_use]
    pub fn gross(&self) -> Money {
        Money::new(
            self.tax_summary().iter().map(|t| t.gross).sum(),
            self.currency,
        )
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
/// use emob_core::{Currency, Energy};
/// use rust_decimal::Decimal;
/// use std::str::FromStr;
/// use time::macros::datetime;
///
/// # let dec = |s: &str| Decimal::from_str(s).unwrap();
/// let tariff = Tariff::simple(
///     "ad-hoc".parse()?,
///     Currency::EUR,
///     TariffKind::AdHoc,
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
    let mut notes = preflight(tariff, session);

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
    for period in &periods {
        let state = SessionState {
            energy_kwh: cumulative_energy,
            elapsed_seconds,
            at: period.start,
            power_kw: period.average_power_kw(),
        };
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
                Dimension::Time if period.charging => seconds,
                Dimension::ParkingTime if !period.charging => seconds,
                Dimension::Time | Dimension::ParkingTime => Decimal::ZERO,
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
                accumulate(&mut accumulators, component, quantity);
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

    // Block rounding applies to what was actually billed for a price, not to
    // each period of it — rounding every quarter hour up to a block would bill
    // a two-hour session eight times over.
    let mut lines: Vec<Line> = accumulators
        .into_iter()
        .map(|acc| {
            let billed = apply_step(acc.dimension, acc.step_size, acc.quantity, &mut notes);
            // Multiply, then divide. `price × seconds / 3600` is exact wherever
            // the arithmetic allows it to be; `price × (seconds / 3600)` has
            // already lost the last digits to a repeating decimal — which is
            // why `base_quantity` is the figure the amount comes from and
            // `quantity` is the one a driver reads.
            let per_display_unit = Line::base_units_per_unit(acc.dimension);
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

    let lines_total: Decimal = lines.iter().map(|l| l.amount).sum();
    let adjustment = bound(tariff, lines_total, adjustment_vat(tariff, &lines));
    if let Some(adjustment) = adjustment {
        notes.push(RatingNote::Adjusted(adjustment));
    }

    Rated {
        lines,
        currency: tariff.currency,
        tax_included: tariff.tax_included,
        adjustment,
        notes,
    }
}

/// What is worth saying about a tariff and a session **before** either is
/// touched — facts about the two documents rather than about the arithmetic.
fn preflight(tariff: &Tariff, session: &Chargeable) -> Vec<RatingNote> {
    let mut notes = Vec::new();

    for (index, element) in tariff.elements.iter().enumerate() {
        if !element.restrictions.is_evaluable() {
            notes.push(RatingNote::UnevaluableRestriction {
                index,
                restrictions: element.restrictions.unevaluable.clone(),
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

    // A time-of-day restriction is read against the offset the period carries,
    // because that is all an `OffsetDateTime` knows. Every period of a session
    // this workspace assembles carries one; a session assembled from somebody
    // else's timestamps need not, and then the cuts are all in the first one's
    // frame. Said once, here, rather than left for a driver to find on a
    // clock-change night.
    if tariff
        .elements
        .iter()
        .any(|element| element.restrictions.reads_the_wall_clock())
        && let Some(first) = session.periods().first()
    {
        let judged_in = first.start.offset();
        if let Some(other) = session
            .periods()
            .iter()
            .flat_map(|period| [period.start.offset(), period.end.offset()])
            .find(|offset| *offset != judged_in)
        {
            notes.push(RatingNote::MixedUtcOffsets {
                judged_in_seconds: judged_in.whole_seconds(),
                also_seen_seconds: other.whole_seconds(),
            });
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
/// interpolated. Splitting a quarter hour at a kilowatt-hour boundary — or a
/// kilowatt-hour at a clock boundary — assumes constant power across it, which a
/// tapering charge curve does not deliver; the residual is under a second of a
/// per-minute fee, and the alternative — sub-second period boundaries — would
/// lose whole seconds to `whole_seconds()` and stop the durations summing.
fn subdivide_at_thresholds(tariff: &Tariff, periods: &[Period]) -> Vec<Period> {
    let Thresholds {
        energy: energy_thresholds,
        duration: duration_thresholds,
        clock: clock_thresholds,
        date: date_thresholds,
    } = Thresholds::of(tariff);
    if energy_thresholds.is_empty()
        && duration_thresholds.is_empty()
        && clock_thresholds.is_empty()
        && date_thresholds.is_empty()
    {
        return periods.to_vec();
    }

    let mut out = Vec::with_capacity(periods.len());
    let mut cumulative_energy = Decimal::ZERO;
    let mut elapsed_seconds: u64 = 0;

    for period in periods {
        let seconds = period.seconds();
        let energy = period.energy.kwh();

        // A period with no duration cannot be cut in time, and one that
        // delivered nothing has no energy boundary inside it.
        let mut cuts: Vec<(u64, Decimal)> = Vec::new();
        if seconds > 0 {
            // A duration threshold and a wall-clock threshold are the same kind
            // of cut — an offset into the period — and the energy at either is
            // interpolated the same way.
            let by_offset = |offset: u64, cuts: &mut Vec<(u64, Decimal)>| {
                if offset > 0 && offset < seconds {
                    // Multiply before dividing, as everywhere else here.
                    let at_cut =
                        cumulative_energy + energy * Decimal::from(offset) / Decimal::from(seconds);
                    cuts.push((offset, at_cut));
                }
            };

            for &threshold in &duration_thresholds {
                let Some(offset) = threshold.checked_sub(elapsed_seconds) else {
                    continue;
                };
                by_offset(offset, &mut cuts);
            }
            for offset in clock_cut_offsets(&clock_thresholds, &date_thresholds, period) {
                by_offset(offset, &mut cuts);
            }
            if energy > Decimal::ZERO {
                for &threshold in &energy_thresholds {
                    let delta = threshold - cumulative_energy;
                    if delta > Decimal::ZERO && delta < energy {
                        let offset = seconds_for(delta, energy, seconds);
                        cuts.push((offset, threshold));
                    }
                }
            }
        }

        // Only cuts that advance **both** the clock and the register survive.
        // An energy threshold and a duration threshold can land in the same
        // second and disagree about how much had been delivered by it, and a
        // piece whose energy ran backwards would be a negative quantity — so
        // the sweep keeps the first of any such pair and drops the rest. The
        // window it gives up is under a second either way.
        cuts.sort_unstable();
        let ceiling = cumulative_energy + energy;
        let mut boundary = (0_u64, cumulative_energy);
        cuts.retain(|&(offset, at_cut)| {
            let usable = offset > boundary.0
                && offset < seconds
                && at_cut >= boundary.1
                && at_cut <= ceiling;
            if usable {
                boundary = (offset, at_cut);
            }
            usable
        });

        // A period nothing cuts is passed through untouched, so nothing can
        // drift: `seconds` truncates, and rebuilding a whole period out of
        // whole seconds would lose the sub-second remainder of one that has
        // one.
        if cuts.is_empty() {
            out.push(period.clone());
            cumulative_energy = ceiling;
            elapsed_seconds += seconds;
            continue;
        }

        // Differences of cumulative values, so the pieces telescope back to the
        // period's own energy exactly. The last piece ends at the period's own
        // end rather than at a whole-second offset, for the same reason.
        let mut previous = (0_u64, cumulative_energy);
        for &(offset, at_cut) in &cuts {
            out.push(Period {
                start: period.start + time::Duration::seconds(i64_of(previous.0)),
                end: period.start + time::Duration::seconds(i64_of(offset)),
                // Non-negative by the sweep above; the clamp is unreachable and
                // is here so this cannot become a panic if that ever changes.
                energy: Energy::from_kwh((at_cut - previous.1).max(Decimal::ZERO))
                    .unwrap_or(Energy::ZERO),
                charging: period.charging,
            });
            previous = (offset, at_cut);
        }
        out.push(Period {
            start: period.start + time::Duration::seconds(i64_of(previous.0)),
            end: period.end,
            energy: Energy::from_kwh((ceiling - previous.1).max(Decimal::ZERO))
                .unwrap_or(Energy::ZERO),
            charging: period.charging,
        });

        cumulative_energy = ceiling;
        elapsed_seconds += seconds;
    }

    // The property the whole function rests on: cutting a session divides it,
    // it does not change it.
    debug_assert_eq!(
        out.iter().map(|p| p.energy).sum::<Energy>().kwh(),
        periods.iter().map(|p| p.energy).sum::<Energy>().kwh(),
        "subdividing at thresholds must conserve the session's energy"
    );
    out
}

/// The offsets, in whole seconds from the period's start, at which a wall-clock
/// restriction changes which element applies.
///
/// Computed in the period's own UTC offset, because [`matches_restrictions`]
/// judges the times and dates against `state.at` — a cut in any other frame
/// would split a period into two pieces that price identically and leave the
/// real boundary uncut. Every day the period spans is walked: an overnight
/// session crosses `22:00` and `06:00` on different dates.
fn clock_cut_offsets(times: &[time::Time], dates: &[time::Date], period: &Period) -> Vec<u64> {
    // A corruption guard rather than a rule — a period spanning more than a year
    // is a clock fault, and the same bound the session split refuses one at.
    const MAX_DAYS: u16 = 366;

    if times.is_empty() && dates.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let offset = period.start.offset();
    let mut push_if_inside = |candidate: time::OffsetDateTime| {
        if candidate > period.start
            && candidate < period.end
            && let Ok(seconds) = u64::try_from((candidate - period.start).whole_seconds())
        {
            out.push(seconds);
        }
    };

    // A date restriction turns on at midnight of the date it names, in the same
    // local frame. The `push_if_inside` filter does the rest, so a date outside
    // the period costs one comparison and contributes nothing.
    for &date in dates {
        push_if_inside(time::OffsetDateTime::new_in_offset(
            date,
            time::Time::MIDNIGHT,
            offset,
        ));
    }

    // Every day the period touches, in its own local calendar.
    let last = period.end.to_offset(offset).date();
    let mut date = period.start.date();
    for _ in 0..MAX_DAYS {
        for &clock in times {
            push_if_inside(time::OffsetDateTime::new_in_offset(date, clock, offset));
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
fn seconds_for(delta: Decimal, energy: Decimal, seconds: u64) -> u64 {
    use rust_decimal::prelude::ToPrimitive as _;
    (Decimal::from(seconds) * delta / energy)
        .round()
        .to_u64()
        .unwrap_or(0)
        .clamp(0, seconds)
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

fn accumulate(into: &mut Vec<Accumulator>, component: &PriceComponent, quantity: Decimal) {
    if let Some(existing) = into.iter_mut().find(|a| {
        a.dimension == component.dimension && a.price == component.price && a.vat == component.vat
    }) {
        existing.quantity += quantity;
        // A tariff that prices the same dimension twice at one price with two
        // block sizes is a tariff whose author meant the larger one.
        existing.step_size = existing.step_size.max(component.step_size);
    } else {
        into.push(Accumulator {
            dimension: component.dimension,
            price: component.price,
            vat: component.vat,
            step_size: component.step_size,
            quantity,
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

/// Apply the tariff's minimum and maximum to the line total.
fn bound(tariff: &Tariff, lines_total: Decimal, vat: Option<Decimal>) -> Option<Adjustment> {
    if let Some(minimum) = tariff.min_price
        && lines_total < minimum
    {
        return Some(Adjustment {
            kind: AdjustmentKind::Minimum,
            lines_total,
            amount: minimum - lines_total,
            vat,
        });
    }
    if let Some(maximum) = tariff.max_price
        && lines_total > maximum
    {
        return Some(Adjustment {
            kind: AdjustmentKind::Maximum,
            lines_total,
            amount: maximum - lines_total,
            vat,
        });
    }
    None
}

/// Round an accumulated quantity up to a component's block size.
///
/// `actual` is in the dimension's base unit, and so is `step_size`: one Wh for
/// energy — hence the factor of a thousand, which is exact in decimal — and one
/// second for time. Rounding *up* is what the field does and it is always
/// against the customer, which is why it produces a note.
fn apply_step(
    dimension: Dimension,
    step_size: u32,
    actual: Decimal,
    notes: &mut Vec<RatingNote>,
) -> Decimal {
    if step_size <= 1 || dimension == Dimension::Flat {
        return actual;
    }
    let per_base_unit = match dimension {
        Dimension::Energy => WH_PER_KWH,
        Dimension::Time | Dimension::ParkingTime => Decimal::ONE,
        Dimension::Flat => return actual,
    };
    let step = Decimal::from(step_size);
    let blocks = (actual * per_base_unit / step).ceil();
    let billed = blocks * step / per_base_unit;

    if billed != actual {
        // The note reports the displayed unit, because that is what a driver
        // reads: "0.58 h rounded up to 1 h", not "2100 s rounded to 3600 s".
        let to_display = match dimension {
            Dimension::Time | Dimension::ParkingTime => SECONDS_PER_HOUR,
            Dimension::Energy | Dimension::Flat => Decimal::ONE,
        };
        notes.push(RatingNote::RoundedToBlock {
            dimension,
            actual: actual / to_display,
            billed: billed / to_display,
        });
    }
    billed
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

    let date = state.at.date();
    if r.start_date.is_some_and(|from| date < from) || r.end_date.is_some_and(|to| date >= to) {
        return false;
    }

    if !r.days_of_week.is_empty() && !r.days_of_week.contains(&state.at.weekday()) {
        return false;
    }

    let clock = state.at.time();
    match (r.start_time, r.end_time) {
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
    use super::*;
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
            components,
        )
    }

    fn tiered(elements: Vec<TariffElement>) -> Tariff {
        Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
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
        t.min_price = Some(dec("5.00"));
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
        t.min_price = Some(dec("0.50"));
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
        t.min_price = Some(dec("5.00"));
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
        t.max_price = Some(dec("10.00"));
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
        t.min_price = Some(dec("5.00"));
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
        t.min_price = Some(dec("10.00"));

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

    #[test]
    fn a_session_in_two_offsets_says_which_one_the_clock_was_read_in() {
        // An `OffsetDateTime` knows an offset, not a time zone, so a night rate
        // can only be judged in the offset the period carries. Every period the
        // split produces carries one; a session assembled from a partner's
        // timestamps need not, and then a boundary on the far side of a clock
        // change sits an hour out.
        let night = Tariff {
            id: "n".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
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
        };

        // Europe/Berlin springs forward at 02:00 local on 2026-03-29: the
        // readings either side of it are stamped +01:00 and +02:00.
        let mixed = Chargeable::new(vec![
            Period::charging(
                datetime!(2026-03-29 01:30 +1),
                datetime!(2026-03-29 02:00 +1),
                kwh("5"),
            ),
            Period::charging(
                datetime!(2026-03-29 03:00 +2),
                datetime!(2026-03-29 03:30 +2),
                kwh("5"),
            ),
        ])
        .unwrap();
        let rated = rate(&night, &mixed);
        assert!(
            rated
                .notes
                .iter()
                .any(|n| matches!(n, RatingNote::MixedUtcOffsets { .. })),
            "{:?}",
            rated.notes
        );

        // …and a session in one offset says nothing, because there is nothing
        // to say.
        let single = Chargeable::energy_only(
            kwh("10"),
            datetime!(2026-01-02 23:00 +1),
            datetime!(2026-01-03 01:00 +1),
        )
        .unwrap();
        assert!(
            !rate(&night, &single)
                .notes
                .iter()
                .any(|n| matches!(n, RatingNote::MixedUtcOffsets { .. }))
        );

        // …and neither does a tariff that reads no clock at all.
        let flat = Tariff::simple(
            "f".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            vec![PriceComponent::new(Dimension::Energy, dec("0.49"))],
        );
        assert!(rate(&flat, &mixed).notes.is_empty());
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
}
