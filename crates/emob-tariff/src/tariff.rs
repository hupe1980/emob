//! The tariff: what a session costs, and the only thing allowed to say so.
//!
//! # One object, two jobs
//!
//! A charging tariff has to do two things that platforms usually implement
//! twice: it has to **rate** a finished session, and it has to be **displayed**
//! to a driver before the session starts `[AFIR Art. 5(4)]`. When those come
//! from two places they drift, and the failure mode is a driver charged
//! something other than the price they were shown — which is the
//! price-transparency breach the article exists to prevent.
//!
//! So there is one [`Tariff`]. [`crate::rating::rate`] reads its price
//! components; [`crate::display::describe`] reads the same ones. Neither can
//! quote a number the other does not use, and a test asserts it.
//!
//! # Shape
//!
//! The structure follows OCPI's, because that is what a roaming partner will
//! send and expect: a tariff is a list of *elements*, each a list of *price
//! components* guarded by *restrictions*.
//!
//! Which component prices a period is asked **once per dimension**, not once
//! per period: the first element that carries a component for that dimension
//! *and* whose restrictions match `[OCPI 2.3.0 §Tariff]`. Several elements can
//! therefore be in force at the same instant — one per dimension — which is
//! why the specification advises writing a tariff as one unrestricted default
//! element per dimension after the restricted ones. See
//! [`crate::rating::matching_component`].

use emob_core::{Currency, TariffId};
use rust_decimal::Decimal;

/// What a price component charges for.
///
/// The order of the variants is the order `[AFIR Art. 5(4)]` prescribes for
/// display below 50 kW — per kWh, per minute, per session, then anything else —
/// and [`Ord`] is derived so that sorting a list of components *is* complying
/// with the article. See [`crate::display`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "SCREAMING_SNAKE_CASE"))]
pub enum Dimension {
    /// Energy delivered, priced per kWh.
    Energy,
    /// Time spent charging, priced per hour.
    Time,
    /// Time connected but not charging, priced per hour. The "occupancy fee"
    /// `[AFIR Art. 5(4)]` permits at 50 kW and above, *in addition* to the
    /// energy price.
    ParkingTime,
    /// A fixed amount per session.
    Flat,
}

impl Dimension {
    /// The unit a price for this dimension is quoted in.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::Energy => "kWh",
            Self::Time | Self::ParkingTime => "h",
            Self::Flat => "session",
        }
    }

    /// The dimension's **base** unit — the one every quantity is accumulated
    /// and reconciled in, which is exact.
    ///
    /// Differs from [`Self::unit`] for the two time dimensions: a price is
    /// quoted per hour and a duration is counted in whole seconds, because
    /// 3600 has two factors of three and no scale states twenty-five minutes
    /// as a decimal fraction of an hour.
    #[must_use]
    pub const fn base_unit(self) -> &'static str {
        match self {
            Self::Energy => "kWh",
            Self::Time | Self::ParkingTime => "s",
            Self::Flat => "session",
        }
    }

    /// Whether this dimension prices energy.
    ///
    /// The question `[AFIR Art. 5(4)]` turns on at 50 kW and above: the ad-hoc
    /// price there "shall be based on the price per kWh".
    #[must_use]
    pub const fn is_energy(self) -> bool {
        matches!(self, Self::Energy)
    }
}

/// One priced dimension.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PriceComponent {
    /// What is being charged for.
    pub dimension: Dimension,
    /// The price per unit, in the tariff's currency.
    pub price: Decimal,
    /// The VAT percentage, when one applies.
    pub vat: Option<Decimal>,
    /// The block size the dimension is billed in.
    ///
    /// One Wh for [`Dimension::Energy`], one second for the time dimensions,
    /// meaningless for [`Dimension::Flat`]. OCPI 3.0 removes the field and
    /// advises setting it to 1; [`PriceComponent::new`] does, and a value above
    /// 1 is deliberate rounding *against* the customer that has to be written
    /// out in full.
    pub step_size: u32,
}

impl PriceComponent {
    /// A component with no VAT and no block rounding.
    #[must_use]
    pub const fn new(dimension: Dimension, price: Decimal) -> Self {
        Self {
            dimension,
            price,
            vat: None,
            step_size: 1,
        }
    }

    /// The same component with a VAT percentage.
    #[must_use]
    pub const fn with_vat(mut self, vat: Decimal) -> Self {
        self.vat = Some(vat);
        self
    }

    /// The same component billed in blocks.
    ///
    /// # Panics
    ///
    /// Never. A `step_size` of 0 is coerced to 1, because billing in blocks of
    /// zero is not a rounding policy, it is a division by zero waiting to
    /// happen in whatever reads the field next.
    #[must_use]
    pub const fn with_step_size(mut self, step_size: u32) -> Self {
        self.step_size = if step_size == 0 { 1 } else { step_size };
        self
    }
}

/// When an element applies.
///
/// # The restrictions are cumulative, and that is what makes tiers work
///
/// `min_kwh` and `min_duration_s` are read against what the session has done
/// **so far**, not against its final total. That is what OCPI means by "valid
/// from this amount of energy being used", and it is the difference between a
/// tariff that charges the first 10 kWh at one price and the rest at another —
/// which is what the field is for — and one that retroactively reprices the
/// whole session the moment it crosses a threshold, which is what a
/// whole-session reading produces.
///
/// # A restriction this crate cannot evaluate is not an absent restriction
///
/// [`Self::unevaluable`] carries anything a wire adapter parsed and this crate
/// cannot judge — an OCPI `reservation` restriction, a partner extension. An
/// element carrying one **never matches**, and [`crate::rating::rate`] says so
/// in a note. Silently treating it as unrestricted is how a night tariff gets
/// applied at noon.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Restrictions {
    /// Applies only from this time of day, in the offset the session carries.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::clock::option"))]
    pub start_time: Option<time::Time>,
    /// Applies only until this time of day.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::clock::option"))]
    pub end_time: Option<time::Time>,
    /// Applies only from this calendar date.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::date::option"))]
    pub start_date: Option<time::Date>,
    /// Applies only before this calendar date.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::date::option"))]
    pub end_date: Option<time::Date>,
    /// Applies only once this much energy has been delivered, in kWh.
    pub min_kwh: Option<Decimal>,
    /// Applies only below this much energy, in kWh.
    pub max_kwh: Option<Decimal>,
    /// Applies only at or above this average power, in kW.
    pub min_power_kw: Option<Decimal>,
    /// Applies only below this average power, in kW.
    pub max_power_kw: Option<Decimal>,
    /// Applies only from this duration into the session, in seconds.
    pub min_duration_s: Option<u64>,
    /// Applies only below this duration, in seconds.
    pub max_duration_s: Option<u64>,
    /// Applies only on these weekdays. Empty means every day.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::weekday"))]
    pub days_of_week: Vec<time::Weekday>,
    /// Restrictions that arrived on the wire and this crate cannot evaluate.
    ///
    /// Kept verbatim, and disqualifying: an element carrying one never matches.
    pub unevaluable: Vec<String>,
}

impl Restrictions {
    /// Whether anything is restricted at all.
    #[must_use]
    pub fn is_unrestricted(&self) -> bool {
        *self == Self::default()
    }

    /// Whether every restriction here is one this crate can judge.
    #[must_use]
    pub fn is_evaluable(&self) -> bool {
        self.unevaluable.is_empty()
    }

    /// Whether anything here is read against the **wall clock** rather than
    /// against a quantity.
    ///
    /// The distinction matters because a time of day, a weekday and a calendar
    /// date can only be judged in the UTC offset a period carries — an
    /// `OffsetDateTime` knows an offset, not a time zone — while kilowatt-hours,
    /// seconds elapsed and kilowatts do not care. [`crate::rate`] reports a
    /// session whose periods disagree about the offset only when a restriction
    /// actually reads it.
    #[must_use]
    pub fn reads_the_wall_clock(&self) -> bool {
        self.start_time.is_some()
            || self.end_time.is_some()
            || self.start_date.is_some()
            || self.end_date.is_some()
            || !self.days_of_week.is_empty()
    }

    /// A one-line statement of what this element is restricted to, for a price
    /// display that has to admit the price depends on something.
    ///
    /// Empty when the element is unrestricted.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        // `time::Time`'s own `Display` writes seconds and sub-seconds; a price
        // display wants `22:00`.
        let hhmm = |t: time::Time| format!("{:02}:{:02}", t.hour(), t.minute());
        match (self.start_time, self.end_time) {
            (Some(from), Some(to)) => parts.push(format!("{}–{}", hhmm(from), hhmm(to))),
            (Some(from), None) => parts.push(format!("from {}", hhmm(from))),
            (None, Some(to)) => parts.push(format!("until {}", hhmm(to))),
            (None, None) => {}
        }
        if !self.days_of_week.is_empty() {
            let days: Vec<String> = self
                .days_of_week
                .iter()
                .map(|d| d.to_string()[..3].to_owned())
                .collect();
            parts.push(days.join("/"));
        }
        match (self.min_kwh, self.max_kwh) {
            (Some(min), Some(max)) => parts.push(format!("{min}–{max} kWh")),
            (Some(min), None) => parts.push(format!("from {min} kWh")),
            (None, Some(max)) => parts.push(format!("first {max} kWh")),
            (None, None) => {}
        }
        match (self.min_power_kw, self.max_power_kw) {
            (Some(min), Some(max)) => parts.push(format!("{min}–{max} kW")),
            (Some(min), None) => parts.push(format!("from {min} kW")),
            (None, Some(max)) => parts.push(format!("below {max} kW")),
            (None, None) => {}
        }
        match (self.min_duration_s, self.max_duration_s) {
            (Some(min), Some(max)) => parts.push(format!("{}–{} min", min / 60, max / 60)),
            (Some(min), None) => parts.push(format!("after {} min", min / 60)),
            (None, Some(max)) => parts.push(format!("first {} min", max / 60)),
            (None, None) => {}
        }
        match (self.start_date, self.end_date) {
            (Some(from), Some(to)) => parts.push(format!("{from}–{to}")),
            (Some(from), None) => parts.push(format!("from {from}")),
            (None, Some(to)) => parts.push(format!("until {to}")),
            (None, None) => {}
        }
        for unknown in &self.unevaluable {
            parts.push(format!("unevaluable: {unknown}"));
        }
        parts.join(", ")
    }
}

/// A group of price components and the restrictions that gate them.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TariffElement {
    /// What this element charges.
    pub components: Vec<PriceComponent>,
    /// When it applies.
    pub restrictions: Restrictions,
}

impl TariffElement {
    /// An element that always applies.
    #[must_use]
    pub fn unrestricted(components: Vec<PriceComponent>) -> Self {
        Self {
            components,
            restrictions: Restrictions::default(),
        }
    }

    /// The component for a dimension, if this element prices it.
    #[must_use]
    pub fn component(&self, dimension: Dimension) -> Option<&PriceComponent> {
        self.components.iter().find(|c| c.dimension == dimension)
    }
}

/// Whether prices include tax.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "SCREAMING_SNAKE_CASE"))]
pub enum TaxIncluded {
    /// The prices are gross.
    Yes,
    /// The prices are net; VAT is added.
    No,
    /// The party does not know, because no tax regime applies to it.
    NotApplicable,
}

/// Who a tariff is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum TariffKind {
    /// The price a driver with no contract pays at the point.
    ///
    /// The one `[AFIR Art. 5(4)]` regulates: it must be shown before the
    /// session starts, and at 50 kW and above it must be based on a price per
    /// kWh.
    AdHoc,
    /// The price under a contract with an e-mobility provider.
    Contract,
}

/// A tariff.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Tariff {
    /// Which tariff.
    pub id: TariffId,
    /// The currency every price in it is quoted in.
    pub currency: Currency,
    /// Who it is for.
    pub kind: TariffKind,
    /// Whether the prices include tax.
    pub tax_included: TaxIncluded,
    /// The elements, in the order they are tried — **per dimension**
    /// `[OCPI 2.3.0 §Tariff]`, so an element pricing only a session fee does
    /// not end the search for a price per kWh. Cardinality `+`.
    pub elements: Vec<TariffElement>,
    /// A session under this tariff costs at least this much, in the basis
    /// [`Self::tax_included`] states — gross under [`TaxIncluded::Yes`], net
    /// under [`TaxIncluded::No`].
    ///
    /// One figure rather than OCPI's pair of a before-tax and an after-tax
    /// bound, because a tariff whose components carry one VAT rate has one
    /// answer and a tariff whose components carry several has no single
    /// taxable amount to bound. The rate the adjustment lands in is a field on
    /// [`crate::Adjustment`] rather than an assumption inside a sum.
    pub min_price: Option<Decimal>,
    /// A session under this tariff costs at most this much, in the same basis
    /// as [`Self::min_price`].
    pub max_price: Option<Decimal>,
    /// The first instant this version of the tariff is in force, **inclusive**.
    ///
    /// `None` for a tariff that has always been in force.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339::option"))]
    pub valid_from: Option<time::OffsetDateTime>,
    /// The instant it stops being in force, **exclusive**.
    ///
    /// Half-open on purpose, and for the reason the key registry's windows are:
    /// with two inclusive bounds a tariff replaced at midnight has two versions
    /// covering that instant, and the answer depends on insertion order.
    /// `[from, until)` makes consecutive versions partition the timeline
    /// exactly, which is what a price history is.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339::option"))]
    pub valid_until: Option<time::OffsetDateTime>,
}

impl Tariff {
    /// A tariff with one unrestricted element.
    #[must_use]
    pub fn simple(
        id: TariffId,
        currency: Currency,
        kind: TariffKind,
        components: Vec<PriceComponent>,
    ) -> Self {
        Self {
            id,
            currency,
            kind,
            tax_included: TaxIncluded::Yes,
            elements: vec![TariffElement::unrestricted(components)],
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        }
    }

    /// The same tariff, in force over a half-open window.
    #[must_use]
    pub const fn valid_between(
        mut self,
        from: Option<time::OffsetDateTime>,
        until: Option<time::OffsetDateTime>,
    ) -> Self {
        self.valid_from = from;
        self.valid_until = until;
        self
    }

    /// Whether this version was in force at an instant — `[from, until)`.
    #[must_use]
    pub fn covers(&self, at: time::OffsetDateTime) -> bool {
        self.valid_from.is_none_or(|from| at >= from)
            && self.valid_until.is_none_or(|until| at < until)
    }

    /// Every dimension this tariff prices anywhere.
    #[must_use]
    pub fn dimensions(&self) -> Vec<Dimension> {
        let mut found: Vec<Dimension> = self
            .elements
            .iter()
            .flat_map(|e| e.components.iter().map(|c| c.dimension))
            .collect();
        found.sort_unstable();
        found.dedup();
        found
    }

    /// Whether any element prices energy.
    ///
    /// The `[AFIR Art. 5(4)]` question at 50 kW and above.
    #[must_use]
    pub fn prices_energy(&self) -> bool {
        self.dimensions().iter().any(|d| d.is_energy())
    }

    /// The VAT rate that governs the whole tariff, when its components agree.
    ///
    /// `None` when different components carry different rates — which is
    /// lawful (a service fee and delivered electricity can sit in different
    /// categories) and means a caller wanting one number has to ask for the
    /// breakdown instead.
    #[must_use]
    pub fn uniform_vat(&self) -> Option<Decimal> {
        let mut rates = self
            .elements
            .iter()
            .flat_map(|e| e.components.iter().map(|c| c.vat));
        let first = rates.next()?;
        if rates.all(|r| r == first) {
            first
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::prelude::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn dimensions_order_the_way_afir_prescribes() {
        // Sorting the components *is* complying with the display order, so the
        // ordering is a property of the type rather than of a formatter.
        let mut ds = vec![
            Dimension::Flat,
            Dimension::ParkingTime,
            Dimension::Energy,
            Dimension::Time,
        ];
        ds.sort_unstable();
        assert_eq!(
            ds,
            vec![
                Dimension::Energy,
                Dimension::Time,
                Dimension::ParkingTime,
                Dimension::Flat
            ]
        );
    }

    #[test]
    fn a_zero_step_size_becomes_one() {
        // Billing in blocks of zero is a division waiting to happen.
        let c = PriceComponent::new(Dimension::Energy, dec("0.49")).with_step_size(0);
        assert_eq!(c.step_size, 1);
    }

    #[test]
    fn a_tariff_knows_whether_it_prices_energy() {
        let energy = Tariff::simple(
            "t1".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            vec![PriceComponent::new(Dimension::Energy, dec("0.49"))],
        );
        assert!(energy.prices_energy());

        let by_the_minute = Tariff::simple(
            "t2".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            vec![PriceComponent::new(Dimension::Time, dec("0.10"))],
        );
        assert!(!by_the_minute.prices_energy());
    }

    #[test]
    fn dimensions_are_reported_once_each_and_in_order() {
        let t = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            tax_included: TaxIncluded::Yes,
            elements: vec![
                TariffElement::unrestricted(vec![
                    PriceComponent::new(Dimension::Flat, dec("0.50")),
                    PriceComponent::new(Dimension::Energy, dec("0.49")),
                ]),
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
        assert_eq!(t.dimensions(), vec![Dimension::Energy, Dimension::Flat]);
    }

    #[test]
    fn units_are_the_ones_prices_are_quoted_in() {
        assert_eq!(Dimension::Energy.unit(), "kWh");
        assert_eq!(Dimension::Time.unit(), "h");
        assert_eq!(Dimension::Flat.unit(), "session");
    }
}
