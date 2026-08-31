//! Turning a session into money.
//!
//! # What is charged, and what it is charged against
//!
//! Four dimensions, each with its own quantity:
//!
//! | Dimension | Quantity | Unit |
//! |---|---|---|
//! | `Energy` | delivered energy | kWh |
//! | `Time` | time charging | hours |
//! | `ParkingTime` | time connected but not charging | hours |
//! | `Flat` | the session itself | once |
//!
//! # Every number the total is made of is kept
//!
//! [`rate`] returns a [`Rated`] carrying one [`Line`] per component that
//! applied, each with its quantity, its unit price and its amount. The total is
//! the sum of the lines, and nothing else — there is no term in the total that
//! is not a line, so "why is this €14.46" is answerable by reading the
//! structure rather than by re-deriving it.
//!
//! That is the same rule the sibling `hems` workspace applies to its optimiser:
//! every term the plan may spend is a term the report charges.
//!
//! # Rounding happens once, at the end
//!
//! Each line is computed exactly and kept exact. Only [`Rated::total`] rounds,
//! to the currency's minor unit, half away from zero. Rounding per line and
//! then summing gives a different answer, and which of the two is correct is a
//! tax question rather than an arithmetic one — so the exact figures survive
//! and the caller can do either.

use emob_core::Energy;
use emob_core::quantity::{Currency, Money};
use rust_decimal::Decimal;

use crate::tariff::{Dimension, PriceComponent, Restrictions, Tariff, TariffElement};

/// What a session did, in the terms a tariff prices.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Chargeable {
    /// Energy delivered.
    pub energy: Energy,
    /// Seconds spent charging.
    pub charging_seconds: u64,
    /// Seconds connected but not charging.
    pub parking_seconds: u64,
    /// When the session started, local to the station — what a time-of-day
    /// restriction is evaluated against.
    pub started_at: time::OffsetDateTime,
}

impl Chargeable {
    /// A session that only delivered energy.
    #[must_use]
    pub const fn energy_only(energy: Energy, started_at: time::OffsetDateTime) -> Self {
        Self {
            energy,
            charging_seconds: 0,
            parking_seconds: 0,
            started_at,
        }
    }

    /// The quantity a dimension is priced against, in the dimension's own unit.
    #[must_use]
    pub fn quantity(&self, dimension: Dimension) -> Decimal {
        match dimension {
            Dimension::Energy => self.energy.kwh(),
            Dimension::Time => seconds_to_hours(self.charging_seconds),
            Dimension::ParkingTime => seconds_to_hours(self.parking_seconds),
            Dimension::Flat => Decimal::ONE,
        }
    }
}

/// Seconds as an exact fraction of an hour.
///
/// `Decimal` division by 3600 is exact for any second count that divides it and
/// carries 28 significant digits otherwise, which is far beyond what a price
/// can express — so no hour is lost the way `seconds as f64 / 3600.0` loses
/// one.
fn seconds_to_hours(seconds: u64) -> Decimal {
    Decimal::from(seconds) / Decimal::from(3600)
}

/// One priced line of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Line {
    /// What was charged for.
    pub dimension: Dimension,
    /// How much of it, in the dimension's unit — after any block rounding.
    pub quantity: Decimal,
    /// The price per unit that was applied.
    pub unit_price: Decimal,
    /// The amount, exact and unrounded.
    pub amount: Decimal,
    /// The VAT percentage, when the component carried one.
    pub vat: Option<Decimal>,
}

/// Something the rating had to assume.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RatingNote {
    /// No element's restrictions matched, so nothing was charged.
    NoMatchingElement,
    /// A quantity was rounded up to the component's block size.
    RoundedToBlock {
        /// Which dimension.
        dimension: Dimension,
        /// What was actually used.
        actual: Decimal,
        /// What was billed.
        billed: Decimal,
    },
    /// The total was raised to the tariff's minimum.
    RaisedToMinimum {
        /// What the lines came to.
        lines: Decimal,
        /// The minimum applied.
        minimum: Decimal,
    },
    /// The total was capped at the tariff's maximum.
    CappedAtMaximum {
        /// What the lines came to.
        lines: Decimal,
        /// The cap applied.
        maximum: Decimal,
    },
}

impl core::fmt::Display for RatingNote {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoMatchingElement => {
                write!(
                    f,
                    "no tariff element matched this session; nothing was charged"
                )
            }
            Self::RoundedToBlock {
                dimension,
                actual,
                billed,
            } => write!(
                f,
                "{dimension:?} rounded up from {actual} to {billed} {}",
                dimension.unit()
            ),
            Self::RaisedToMinimum { lines, minimum } => {
                write!(
                    f,
                    "the lines came to {lines}, raised to the minimum {minimum}"
                )
            }
            Self::CappedAtMaximum { lines, maximum } => {
                write!(
                    f,
                    "the lines came to {lines}, capped at the maximum {maximum}"
                )
            }
        }
    }
}

/// A rated session.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Rated {
    /// One line per component that applied.
    pub lines: Vec<Line>,
    /// The currency.
    pub currency: Currency,
    /// Anything the rating had to assume.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub notes: Vec<RatingNote>,
    /// The total after any minimum or maximum, exact and unrounded.
    subtotal: Decimal,
}

impl Rated {
    /// The exact total, before rounding to the minor unit.
    #[must_use]
    pub const fn exact_total(&self) -> Money {
        Money::new(self.subtotal, self.currency)
    }

    /// The total, rounded to the currency's minor unit.
    #[must_use]
    pub fn total(&self) -> Money {
        self.exact_total().round_to_minor_unit()
    }

    /// Whether the lines sum to the total.
    ///
    /// False exactly when a minimum or maximum moved it, and then the note says
    /// which. There is no other way for the two to differ: the invariant is
    /// that **every term of the total is a line, or a note**.
    #[must_use]
    pub fn lines_sum_to_total(&self) -> bool {
        self.lines.iter().map(|l| l.amount).sum::<Decimal>() == self.subtotal
    }

    /// The amount charged for one dimension.
    #[must_use]
    pub fn amount_for(&self, dimension: Dimension) -> Option<Decimal> {
        self.lines
            .iter()
            .find(|l| l.dimension == dimension)
            .map(|l| l.amount)
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
/// );
///
/// let rated = rate(&tariff, &session);
/// assert_eq!(rated.total().to_string(), "14.46 EUR");
/// assert!(rated.lines_sum_to_total());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn rate(tariff: &Tariff, session: &Chargeable) -> Rated {
    let mut notes = Vec::new();

    let Some(element) = matching_element(tariff, session) else {
        notes.push(RatingNote::NoMatchingElement);
        return Rated {
            lines: Vec::new(),
            currency: tariff.currency,
            notes,
            subtotal: Decimal::ZERO,
        };
    };

    let mut lines = Vec::new();
    // Sorted so the lines come out in the order `[AFIR Art. 5(4)]` prescribes,
    // which means an invoice and a price display list the same things the same
    // way round without either having to know about the other.
    let mut components: Vec<&PriceComponent> = element.components.iter().collect();
    components.sort_by_key(|c| c.dimension);

    for component in components {
        let actual = session.quantity(component.dimension);
        if actual.is_zero() && component.dimension != Dimension::Flat {
            continue;
        }
        let billed = apply_step(component, actual, &mut notes);
        lines.push(Line {
            dimension: component.dimension,
            quantity: billed,
            unit_price: component.price,
            amount: billed * component.price,
            vat: component.vat,
        });
    }

    let mut subtotal: Decimal = lines.iter().map(|l| l.amount).sum();

    if let Some(minimum) = tariff.min_price
        && subtotal < minimum
    {
        notes.push(RatingNote::RaisedToMinimum {
            lines: subtotal,
            minimum,
        });
        subtotal = minimum;
    }
    if let Some(maximum) = tariff.max_price
        && subtotal > maximum
    {
        notes.push(RatingNote::CappedAtMaximum {
            lines: subtotal,
            maximum,
        });
        subtotal = maximum;
    }

    Rated {
        lines,
        currency: tariff.currency,
        notes,
        subtotal,
    }
}

/// Round a quantity up to the component's block size.
///
/// The block is expressed in the dimension's base unit — one Wh for energy, one
/// second for time — so the arithmetic converts into that unit, rounds up, and
/// converts back. Rounding *up* is what the field does and it is always against
/// the customer, which is why it produces a note.
fn apply_step(component: &PriceComponent, actual: Decimal, notes: &mut Vec<RatingNote>) -> Decimal {
    if component.step_size <= 1 || component.dimension == Dimension::Flat {
        return actual;
    }
    let per_base_unit = match component.dimension {
        Dimension::Energy => Decimal::from(1000), // kWh → Wh
        Dimension::Time | Dimension::ParkingTime => Decimal::from(3600), // h → s
        Dimension::Flat => return actual,
    };
    let step = Decimal::from(component.step_size);
    let in_base = actual * per_base_unit;
    let blocks = (in_base / step).ceil();
    let billed = blocks * step / per_base_unit;

    if billed != actual {
        notes.push(RatingNote::RoundedToBlock {
            dimension: component.dimension,
            actual,
            billed,
        });
    }
    billed
}

/// The first element whose restrictions the session satisfies.
fn matching_element<'a>(tariff: &'a Tariff, session: &Chargeable) -> Option<&'a TariffElement> {
    tariff.elements.iter().find(|e| element_matches(e, session))
}

/// Whether an element's restrictions admit a session.
///
/// Public so [`crate::display`] selects the element by *this* rule rather than
/// a parallel one. Two implementations of "which element applies" is exactly
/// the drift this crate exists to prevent, one level down.
#[must_use]
pub fn element_matches(element: &TariffElement, session: &Chargeable) -> bool {
    matches_restrictions(&element.restrictions, session)
}

fn matches_restrictions(r: &Restrictions, session: &Chargeable) -> bool {
    if r.is_unrestricted() {
        return true;
    }

    let kwh = session.energy.kwh();
    if r.min_kwh.is_some_and(|min| kwh < min) || r.max_kwh.is_some_and(|max| kwh >= max) {
        return false;
    }

    let duration = session.charging_seconds + session.parking_seconds;
    if r.min_duration_s.is_some_and(|min| duration < min)
        || r.max_duration_s.is_some_and(|max| duration >= max)
    {
        return false;
    }

    if !r.days_of_week.is_empty() && !r.days_of_week.contains(&session.started_at.weekday()) {
        return false;
    }

    let clock = session.started_at.time();
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

    fn ad_hoc(components: Vec<PriceComponent>) -> Tariff {
        Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            components,
        )
    }

    fn session(energy: &str) -> Chargeable {
        Chargeable::energy_only(kwh(energy), datetime!(2026-01-02 10:00 +1))
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
    fn every_term_of_the_total_is_a_line() {
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::Flat, dec("0.50")),
            PriceComponent::new(Dimension::Time, dec("0.06")),
        ]);
        let mut s = session("10");
        s.charging_seconds = 1800;

        let r = rate(&t, &s);
        assert_eq!(r.lines.len(), 3);
        assert!(r.lines_sum_to_total());
        assert_eq!(
            r.lines.iter().map(|l| l.amount).sum::<Decimal>(),
            r.exact_total().amount()
        );
    }

    #[test]
    fn lines_come_out_in_the_order_afir_prescribes() {
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Flat, dec("0.50")),
            PriceComponent::new(Dimension::ParkingTime, dec("0.10")),
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::Time, dec("0.06")),
        ]);
        let mut s = session("10");
        s.charging_seconds = 1800;
        s.parking_seconds = 600;

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
        // No parking time at all: no parking line, rather than a line of zero.
        let r = rate(&t, &session("10"));
        assert_eq!(r.amount_for(Dimension::ParkingTime), None);
        assert_eq!(r.amount_for(Dimension::Flat), Some(dec("0.50")));
    }

    #[test]
    fn seconds_become_exact_hours() {
        let t = ad_hoc(vec![PriceComponent::new(Dimension::Time, dec("6.00"))]);
        let mut s = session("0");
        s.charging_seconds = 3600;
        assert_eq!(rate(&t, &s).exact_total().amount(), dec("6.00"));

        s.charging_seconds = 1800;
        assert_eq!(rate(&t, &s).exact_total().amount(), dec("3.000"));
    }

    #[test]
    fn block_rounding_is_reported_because_it_favours_the_operator() {
        // A kWh billed in blocks of 1000 Wh: 10.4 kWh becomes 11.
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.50")).with_step_size(1000),
        ]);
        let r = rate(&t, &session("10.4"));

        assert_eq!(r.lines[0].quantity, dec("11"));
        assert_eq!(r.exact_total().amount(), dec("5.50"));
        assert!(
            r.notes
                .iter()
                .any(|n| matches!(n, RatingNote::RoundedToBlock { .. })),
            "rounding up is always against the customer, so it is said out loud"
        );
        assert!(r.notes[0].to_string().contains("10.4"));
    }

    #[test]
    fn a_step_size_of_one_rounds_nothing() {
        let t = ad_hoc(vec![PriceComponent::new(Dimension::Energy, dec("0.50"))]);
        let r = rate(&t, &session("10.4"));
        assert_eq!(r.lines[0].quantity, dec("10.4"));
        assert!(r.notes.is_empty());
    }

    #[test]
    fn a_minimum_moves_the_total_and_says_so() {
        let mut t = ad_hoc(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        t.min_price = Some(dec("5.00"));
        let r = rate(&t, &session("1"));

        assert_eq!(r.total().amount(), dec("5.00"));
        assert!(!r.lines_sum_to_total(), "and the report admits it");
        assert!(
            r.notes
                .iter()
                .any(|n| matches!(n, RatingNote::RaisedToMinimum { .. }))
        );
    }

    #[test]
    fn a_maximum_caps_the_total_and_says_so() {
        let mut t = ad_hoc(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        t.max_price = Some(dec("10.00"));
        let r = rate(&t, &session("100"));

        assert_eq!(r.total().amount(), dec("10.00"));
        assert!(
            r.notes
                .iter()
                .any(|n| matches!(n, RatingNote::CappedAtMaximum { .. }))
        );
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
        let t = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            tax_included: TaxIncluded::Yes,
            elements: vec![night, day],
            min_price: None,
            max_price: None,
        };

        let at_23 = Chargeable::energy_only(kwh("10"), datetime!(2026-01-02 23:00 +1));
        assert_eq!(rate(&t, &at_23).lines[0].unit_price, dec("0.29"));

        let at_03 = Chargeable::energy_only(kwh("10"), datetime!(2026-01-02 03:00 +1));
        assert_eq!(rate(&t, &at_03).lines[0].unit_price, dec("0.29"));

        let at_noon = Chargeable::energy_only(kwh("10"), datetime!(2026-01-02 12:00 +1));
        assert_eq!(rate(&t, &at_noon).lines[0].unit_price, dec("0.49"));
    }

    #[test]
    fn energy_restrictions_select_the_element() {
        let first_ten = TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.39"))],
            restrictions: Restrictions {
                max_kwh: Some(dec("10")),
                ..Restrictions::default()
            },
        };
        let rest =
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.59"))]);
        let t = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            tax_included: TaxIncluded::Yes,
            elements: vec![first_ten, rest],
            min_price: None,
            max_price: None,
        };

        assert_eq!(rate(&t, &session("5")).lines[0].unit_price, dec("0.39"));
        assert_eq!(rate(&t, &session("50")).lines[0].unit_price, dec("0.59"));
        // The boundary is exclusive at the top, so 10 kWh falls to the second.
        assert_eq!(rate(&t, &session("10")).lines[0].unit_price, dec("0.59"));
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
        let t = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            tax_included: TaxIncluded::Yes,
            elements: vec![
                weekend,
                TariffElement::unrestricted(vec![PriceComponent::new(
                    Dimension::Energy,
                    dec("0.49"),
                )]),
            ],
            min_price: None,
            max_price: None,
        };

        // 2026-01-02 is a Friday; 2026-01-03 a Saturday.
        let friday = Chargeable::energy_only(kwh("10"), datetime!(2026-01-02 10:00 +1));
        let saturday = Chargeable::energy_only(kwh("10"), datetime!(2026-01-03 10:00 +1));
        assert_eq!(rate(&t, &friday).lines[0].unit_price, dec("0.49"));
        assert_eq!(rate(&t, &saturday).lines[0].unit_price, dec("0.29"));
    }

    #[test]
    fn nothing_matching_charges_nothing_and_says_so() {
        let t = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            tax_included: TaxIncluded::Yes,
            elements: vec![TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.49"))],
                restrictions: Restrictions {
                    min_kwh: Some(dec("100")),
                    ..Restrictions::default()
                },
            }],
            min_price: None,
            max_price: None,
        };
        let r = rate(&t, &session("10"));
        assert!(r.lines.is_empty());
        assert_eq!(r.total().amount(), Decimal::ZERO);
        assert!(r.notes.contains(&RatingNote::NoMatchingElement));
    }
}
