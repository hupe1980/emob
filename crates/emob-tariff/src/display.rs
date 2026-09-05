//! What the driver is shown before the session starts.
//!
//! # The failure this module exists to make impossible
//!
//! `[AFIR Art. 5(4)]` requires the ad-hoc price to be known to the driver
//! before they start, and `[PAngV]` requires it to be indicated correctly. The
//! way platforms breach that is almost never malice: the price on the screen
//! comes from a CMS field somebody typed, and the price on the invoice comes
//! from the tariff engine, and one of them was updated.
//!
//! Here the description is **derived from the tariff that rates**, through the
//! same price-selection function — [`crate::rating::matching_component`] — that
//! [`crate::rating::rate`] uses for a session's first period, asked once per
//! dimension exactly as the rating asks it `[OCPI 2.3.0 §Tariff]`. There is no
//! second source and no parallel rule, so there is nothing to drift from.
//!
//! # Why "the element that applies now" is the wrong question
//!
//! More than one element can be in force at once — one per dimension. The
//! tariff shape OCPI recommends puts a default price component for each
//! dimension in its own unrestricted element, so `{FLAT 0.50}` followed by
//! `{ENERGY 0.49}` has *both* elements live, and a display that picks one
//! shows the driver a session fee and no price per kilowatt-hour. That is a
//! `[PAngV]` breach produced by an off-by-one reading of a specification.
//!
//! # The order is the regulation's, not a designer's
//!
//! Below 50 kW the article prescribes the order in as many words:
//!
//! > The applicable price components shall be presented in the following order:
//! > — price per kWh; — price per minute; — price per session; and — any other
//! > price component that applies.
//!
//! [`Dimension`] is declared in exactly that order and derives [`Ord`], so
//! sorting the components *is* complying. A display that lists them any other
//! way is not a styling choice, it is a breach.
//!
//! # Per minute, not per hour
//!
//! Tariffs are quoted per hour internally, because that is what OCPI carries.
//! The article says "price per minute". The conversion is exact and happens
//! here, once, so nobody is tempted to store the same rate twice in two units.
//!
//! # A tariff with tiers cannot be described by one set of numbers
//!
//! The article asks for "all its price components", and a tariff that charges
//! the first ten kilowatt-hours at one price has two. Showing only the one that
//! applies at the moment of asking is the failure mode that leaves a driver
//! arriving at 21:58 quoted the day rate for a session billed at the night one.
//! So [`PriceDescription`] carries a [`Tier`] per element — each with the
//! conditions in words — alongside the lines that apply right now.

use emob_core::quantity::Currency;
use rust_decimal::Decimal;

use crate::rating::SessionState;
use crate::tariff::{Dimension, Tariff, TariffElement};

/// One line of what the driver is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DisplayLine {
    /// Which component.
    pub dimension: Dimension,
    /// The price, in the unit [`Self::unit`] names.
    pub price: Decimal,
    /// The VAT percentage the component states, when it states one.
    ///
    /// Carried on the line rather than on the description because
    /// `[PAngV §2 Nr. 1]`'s *Arbeitspreis* is a price *"einschließlich der
    /// Umsatzsteuer"* per component — and a tariff whose energy sits at one
    /// rate and whose service fee sits at another has no single rate to gross
    /// the display with. With the rate on the line, [`describe_gross`] can
    /// gross a mixed tariff correctly instead of refusing it.
    pub vat: Option<Decimal>,
}

impl DisplayLine {
    /// The unit the price is quoted in — `kWh`, `min`, or `session`.
    ///
    /// Note `min`, not `h`: the article asks for a price per minute, while OCPI
    /// carries a price per hour. Derived from the dimension rather than stored
    /// beside it, because a unit field and a dimension field are two statements
    /// about one thing and can be made to disagree.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        display_unit(self.dimension)
    }
}

impl core::fmt::Display for DisplayLine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} / {}", self.price, self.unit())
    }
}

/// One element of a tariff, as a driver has to be able to read it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Tier {
    /// The conditions under which these prices apply, in words. Empty when the
    /// element is unrestricted.
    pub condition: String,
    /// The prices, in the order the article prescribes.
    pub lines: Vec<DisplayLine>,
    /// Whether this is the tier that applies at the instant asked about.
    pub applies_now: bool,
}

impl Tier {
    /// A one-line rendering of this tier's prices.
    #[must_use]
    pub fn prices(&self, currency: Currency) -> String {
        self.lines
            .iter()
            .map(|l| format!("{} {currency} / {}", l.price, l.unit()))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// What a driver must be able to see before starting.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PriceDescription {
    /// The lines that apply at the instant asked about, in the order
    /// `[AFIR Art. 5(4)]` prescribes.
    pub lines: Vec<DisplayLine>,
    /// Every element of the tariff, with its conditions — because "all its
    /// price components" means all of them.
    pub tiers: Vec<Tier>,
    /// The currency.
    pub currency: Currency,
    /// Whether the prices include tax.
    pub tax_included: crate::tariff::TaxIncluded,
    /// The minimum a session will cost, in the basis the prices are quoted in,
    /// when the tariff sets one.
    ///
    /// A driver reads one figure and compares it against the number they were
    /// shown, so this is the bound **in the tariff's own basis** rather than the
    /// pair `[OCPI 2.3.0 §mod_tariffs_pricelimit_class]` carries — see
    /// `PriceLimit::in_basis`. The other limb still binds; it is the rating
    /// that enforces it, and a display that quoted both would be showing a
    /// driver two ceilings to compare one total against.
    pub min_price: Option<Decimal>,
    /// The maximum, in the same basis, when it sets one.
    pub max_price: Option<Decimal>,
}

impl PriceDescription {
    /// The price per kWh that applies now, when the tariff has one.
    #[must_use]
    pub fn per_kwh(&self) -> Option<Decimal> {
        self.line(Dimension::Energy).map(|l| l.price)
    }

    /// The occupancy fee per minute that applies now, when the tariff has one.
    ///
    /// `[AFIR Art. 5(4)]` calls this out separately at 50 kW and above: the fee
    /// a fast charger may add "to discourage long occupancy of the recharging
    /// point".
    #[must_use]
    pub fn occupancy_per_minute(&self) -> Option<Decimal> {
        self.line(Dimension::ParkingTime).map(|l| l.price)
    }

    /// The line for a dimension.
    #[must_use]
    pub fn line(&self, dimension: Dimension) -> Option<&DisplayLine> {
        self.lines.iter().find(|l| l.dimension == dimension)
    }

    /// Whether the price depends on something other than the quantities — a
    /// time of day, a tier, a power band.
    ///
    /// True whenever more than one tier can apply, so a display can say "the
    /// price changes at 22:00" rather than quietly misleading.
    #[must_use]
    pub fn varies_by_condition(&self) -> bool {
        self.tiers.len() > 1
    }

    /// A one-line rendering of the prices that apply now, components in the
    /// prescribed order.
    ///
    /// ```text
    /// 0.49 EUR / kWh · 0.10 EUR / min · 0.50 EUR / session
    /// ```
    #[must_use]
    pub fn one_line(&self) -> String {
        self.lines
            .iter()
            .map(|l| format!("{} {} / {}", l.price, self.currency, l.unit()))
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Every tier, one line each, with its conditions — the full disclosure
    /// `[AFIR Art. 5(4)]` third subparagraph asks for below 50 kW.
    #[must_use]
    pub fn full_disclosure(&self) -> Vec<String> {
        self.tiers
            .iter()
            .map(|tier| {
                if tier.condition.is_empty() {
                    tier.prices(self.currency)
                } else {
                    format!("{} ({})", tier.prices(self.currency), tier.condition)
                }
            })
            .collect()
    }
}

/// Describe what a driver is shown, from the tariff that will rate them.
///
/// `at` selects the tier that applies, because a tariff with time-of-day
/// elements describes differently at different hours — and describing it at a
/// time other than now is exactly what a driver approaching at 21:58 needs.
///
/// ```
/// use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind, describe};
/// use emob_core::{Currency, TimeZone};
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
///     vec![
///         PriceComponent::new(Dimension::Flat, dec("0.50")),
///         PriceComponent::new(Dimension::Energy, dec("0.49")),
///     ],
/// );
///
/// let shown = describe(&tariff, datetime!(2026-01-02 10:00 +1));
/// // Per kWh first, whatever order the components were written in.
/// assert_eq!(shown.one_line(), "0.49 EUR / kWh · 0.50 EUR / session");
/// assert_eq!(shown.per_kwh(), Some(dec("0.49")));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn describe(tariff: &Tariff, at: time::OffsetDateTime) -> PriceDescription {
    // The state a session is in at its first period: nothing delivered, no time
    // elapsed, no power yet. Exactly what `rate` asks the same predicate with,
    // so the tier shown is the tier the first kilowatt-hour is billed at.
    let opening = SessionState::new(&tariff.time_zone, at);

    // One question per dimension, through the rating engine's own selector:
    // a tariff whose session fee and whose kilowatt-hour price sit in two
    // unrestricted elements — the shape `[OCPI 2.3.0 §Tariff]` recommends —
    // has both in force at once, and a display that stops at the first element
    // shows the driver one of them.
    let mut lines: Vec<DisplayLine> = Vec::new();
    let mut applicable: Vec<usize> = Vec::new();
    for dimension in tariff.dimensions() {
        if let Some((index, component)) =
            crate::rating::matching_component(tariff, dimension, &opening)
        {
            lines.push(DisplayLine {
                dimension,
                price: price_in_display_unit(dimension, component.price),
                vat: component.vat,
            });
            applicable.push(index);
        }
    }
    lines.sort_by_key(|l| l.dimension);

    let tiers: Vec<Tier> = tariff
        .elements
        .iter()
        .enumerate()
        .map(|(index, element)| Tier {
            condition: element.restrictions.describe(),
            lines: lines_of(element),
            applies_now: applicable.contains(&index),
        })
        .collect();

    PriceDescription {
        lines,
        tiers,
        currency: tariff.currency,
        tax_included: tariff.tax_included,
        min_price: tariff
            .min_price
            .and_then(|p| p.in_basis(tariff.tax_included)),
        max_price: tariff
            .max_price
            .and_then(|p| p.in_basis(tariff.tax_included)),
    }
}

/// Which bound had no gross spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// The minimum a session costs.
    Minimum,
    /// The maximum.
    Maximum,
}

impl core::fmt::Display for Bound {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Minimum => "minimum",
            Self::Maximum => "maximum",
        })
    }
}

/// Why a net tariff cannot be shown to a consumer as a `Gesamtpreis`.
///
/// **Two absences, so two variants.** A description that could not be grossed
/// because a component states no rate and one that could not be grossed because
/// a *bound* states none are different faults with different remedies, and an
/// `Option` collapsing them would be the shape this workspace keeps finding
/// (rule 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NoGesamtpreis {
    /// A component is quoted net and states no VAT rate, so nothing grosses it
    /// up.
    RateUnstated {
        /// Which component.
        dimension: Dimension,
    },
    /// A bound is stated only before taxes, and the tariff's components state
    /// no single rate to gross it with.
    BoundHasNoGrossSpelling {
        /// Which bound.
        bound: Bound,
    },
}

impl core::fmt::Display for NoGesamtpreis {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::RateUnstated { dimension } => write!(
                f,
                "{dimension:?} is quoted net and states no VAT rate, so it has no Gesamtpreis: \
                 [PAngV §3(1)] and [PAngV §2 Nr. 1] want the price a consumer pays, tax included"
            ),
            Self::BoundHasNoGrossSpelling { bound } => write!(
                f,
                "the {bound} is stated only before taxes and the components state no single rate \
                 to gross it with, so the figure a driver compares their total against cannot be \
                 shown tax-inclusive [PAngV §3(1)]"
            ),
        }
    }
}

impl core::error::Error for NoGesamtpreis {}

impl PriceDescription {
    /// Whether these figures are the **Gesamtpreis** a consumer must be shown.
    ///
    /// `[PAngV §3(1)]` obliges an operator offering a service to consumers to
    /// state *"die Gesamtpreise"*, and `[PAngV §2 Nr. 3]` defines one as the
    /// price payable *"einschließlich der Umsatzsteuer und sonstiger
    /// Preisbestandteile"*. A net figure with a flag beside it is not that: the
    /// driver reads the number.
    ///
    /// [`TaxIncluded::NotApplicable`](crate::tariff::TaxIncluded::NotApplicable)
    /// answers `true`, because outside a tax
    /// regime the figure quoted *is* the figure payable.
    #[must_use]
    pub const fn is_gesamtpreis(&self) -> bool {
        !matches!(self.tax_included, crate::tariff::TaxIncluded::No)
    }
}

/// Describe a tariff as the **Gesamtpreis** a German consumer must be shown.
///
/// # The failure this closes
///
/// [`describe`] hands out the tariff's own figures with a
/// [`TaxIncluded`](crate::tariff::TaxIncluded) beside them. That is right for
/// a partner reconciling a settlement and wrong for the one audience whose law
/// names the number: `[PAngV §14(2)]` wants the *Arbeitspreis*, and
/// `[PAngV §2 Nr. 1]` defines it as the price per kilowatt-hour
/// *"einschließlich der Umsatzsteuer und aller besonderen Verbrauchssteuern"*.
/// A post that renders a net `0.49` breaches `[PAngV §3(1)]` — and every wire
/// crossing in this workspace already refuses to let a net figure travel as a
/// gross one, while the module that faces the driver could not produce the
/// gross one at all.
///
/// # Exact, not rounded
///
/// `0.49` at 19 % is `0.5831` per kWh and this returns `0.5831`. Rounding it
/// here would be a price the operator does not charge, which is the whole
/// failure `[PAngV]` and `[AFIR Art. 5(4)]` are about — and this workspace's
/// standing rule is that the exact figure is what a computation carries and the
/// rounded one is what a document prints. A renderer that shows two decimals
/// rounds at the point of rendering, where the choice is visible.
///
/// # Errors
///
/// [`NoGesamtpreis`] where a net tariff states no rate to gross a component or
/// a bound with. Refused rather than defaulted to zero, because a rate nobody
/// wrote down is not a rate of nothing.
///
/// ```
/// use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind};
/// use emob_tariff::{TaxIncluded, describe, describe_gross};
/// use emob_core::{Currency, TimeZone};
/// use rust_decimal::Decimal;
/// use std::str::FromStr;
/// use time::macros::datetime;
///
/// # let dec = |s: &str| Decimal::from_str(s).unwrap();
/// let mut tariff = Tariff::simple(
///     "ad-hoc".parse()?,
///     Currency::EUR,
///     TariffKind::AdHoc,
///     TimeZone::new("Europe/Berlin")?,
///     vec![PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19"))],
/// );
/// tariff.tax_included = TaxIncluded::No;
///
/// let at = datetime!(2026-01-02 10:00 +1);
/// // What a settlement partner reads — and what a driver may not be shown.
/// assert!(!describe(&tariff, at).is_gesamtpreis());
/// assert_eq!(describe(&tariff, at).one_line(), "0.49 EUR / kWh");
///
/// // What `[PAngV §14(2)]` asks for, exactly.
/// let shown = describe_gross(&tariff, at)?;
/// assert!(shown.is_gesamtpreis());
/// assert_eq!(shown.one_line(), "0.5831 EUR / kWh");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn describe_gross(
    tariff: &Tariff,
    at: time::OffsetDateTime,
) -> Result<PriceDescription, NoGesamtpreis> {
    let shown = describe(tariff, at);
    // Already the figure payable: a gross tariff states it, and outside a tax
    // regime there is nothing to add to it.
    if shown.is_gesamtpreis() {
        return Ok(shown);
    }

    let gross_line = |line: &DisplayLine| -> Result<DisplayLine, NoGesamtpreis> {
        let rate = line.vat.ok_or(NoGesamtpreis::RateUnstated {
            dimension: line.dimension,
        })?;
        Ok(DisplayLine {
            price: line.price * (Decimal::ONE + rate / HUNDRED),
            ..*line
        })
    };

    let lines = shown
        .lines
        .iter()
        .map(gross_line)
        .collect::<Result<Vec<_>, _>>()?;
    let tiers = shown
        .tiers
        .iter()
        .map(|tier| {
            Ok(Tier {
                condition: tier.condition.clone(),
                lines: tier
                    .lines
                    .iter()
                    .map(gross_line)
                    .collect::<Result<Vec<_>, _>>()?,
                applies_now: tier.applies_now,
            })
        })
        .collect::<Result<Vec<_>, NoGesamtpreis>>()?;

    // A bound is a fact about a **total**, so it is grossed with the rate the
    // tariff as a whole states rather than with any one line's — and where the
    // author wrote the gross limb down, that figure wins over any derivation of
    // it `[OCPI 2.3.0 §mod_tariffs_pricelimit_class]`.
    let whole = tariff.vat_basis().stated();
    let gross_bound = |limit: Option<crate::tariff::PriceLimit>,
                       which: Bound|
     -> Result<Option<Decimal>, NoGesamtpreis> {
        let Some(limit) = limit else { return Ok(None) };
        if let Some(after) = limit.after_taxes {
            return Ok(Some(after));
        }
        let Some(before) = limit.before_taxes else {
            return Ok(None);
        };
        let rate = whole.ok_or(NoGesamtpreis::BoundHasNoGrossSpelling { bound: which })?;
        Ok(Some(before * (Decimal::ONE + rate / HUNDRED)))
    };

    Ok(PriceDescription {
        lines,
        tiers,
        currency: shown.currency,
        tax_included: crate::tariff::TaxIncluded::Yes,
        min_price: gross_bound(tariff.min_price, Bound::Minimum)?,
        max_price: gross_bound(tariff.max_price, Bound::Maximum)?,
    })
}

/// One element's prices, in the order the article prescribes.
fn lines_of(element: &TariffElement) -> Vec<DisplayLine> {
    let mut lines: Vec<DisplayLine> = element
        .components
        .iter()
        .map(|c| DisplayLine {
            dimension: c.dimension,
            price: price_in_display_unit(c.dimension, c.price),
            vat: c.vat,
        })
        .collect();
    lines.sort_by_key(|l| l.dimension);
    lines
}

/// The unit a dimension is *displayed* in.
///
/// Time is stored per hour and shown per minute, because the article asks for a
/// price per minute and OCPI carries a price per hour. One conversion, in one
/// place.
const fn display_unit(dimension: Dimension) -> &'static str {
    match dimension {
        Dimension::Energy => "kWh",
        Dimension::Time | Dimension::ParkingTime => "min",
        Dimension::Flat => "session",
    }
}

/// Convert a stored price into its display unit.
pub(crate) fn price_in_display_unit(dimension: Dimension, price: Decimal) -> Decimal {
    match dimension {
        Dimension::Time | Dimension::ParkingTime => price / MINUTES_PER_HOUR,
        Dimension::Energy | Dimension::Flat => price,
    }
}

/// Minutes in an hour.
const MINUTES_PER_HOUR: Decimal = Decimal::from_parts(60, 0, 0, false, 0);

/// The divisor a VAT percentage is a percentage of.
const HUNDRED: Decimal = Decimal::from_parts(100, 0, 0, false, 0);

/// An hourly price as the **exact** price per minute, or `None` when there is
/// none.
///
/// # Why this is a `Option` rather than a division
///
/// `[AFIR Art. 5(4)]` asks for a price per minute; OCPI carries a price per
/// hour; `[OCPP 2.1 Part 2, TariffTimePrice]` carries `priceMinute` and so
/// makes the conversion a *wire* value rather than a display one. A decimal
/// terminates only when its denominator's prime factors are two and five, and
/// sixty carries a three — so an ordinary occupancy fee of €2.50 an hour is
/// €0.041666… a minute and has no exact decimal spelling at all.
///
/// Rounding it states a price the tariff does not charge, which is the
/// display-versus-bill drift this crate exists to make unrepresentable. So the
/// question has one answer or none, and every caller that needs the figure —
/// the display, the AFIR conformance check, and the OCPP 2.1 tariff crossing in
/// `emob-ocpp` — asks it here rather than dividing and hoping. €6.00 an hour is
/// €0.10 a minute; €2.50 an hour is not a price this market can quote.
///
/// ```
/// use emob_tariff::price_per_minute;
/// use rust_decimal::Decimal;
/// # use std::str::FromStr;
/// assert_eq!(
///     price_per_minute(Decimal::from_str("6.00")?),
///     Some(Decimal::from_str("0.10")?)
/// );
/// assert_eq!(price_per_minute(Decimal::from_str("2.50")?), None);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn price_per_minute(per_hour: Decimal) -> Option<Decimal> {
    // The division is done and undone rather than reasoned about in factors: a
    // quotient the arithmetic had to truncate does not multiply back.
    let per_minute = per_hour / MINUTES_PER_HOUR;
    (per_minute * MINUTES_PER_HOUR == per_hour).then_some(per_minute)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rating::{Chargeable, Period, rate};
    use crate::tariff::PriceLimit;
    use crate::tariff::{PriceComponent, Restrictions, TariffElement, TariffKind, TaxIncluded};
    use emob_core::Energy;
    use emob_core::TimeZone;
    use rust_decimal::prelude::FromStr;
    use time::macros::datetime;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn at() -> time::OffsetDateTime {
        datetime!(2026-01-02 10:00 +1)
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

    #[test]
    fn the_displayed_price_is_the_price_that_rates() {
        // The property this module exists for. One tariff, two readers, and
        // they cannot disagree because there is only one set of numbers.
        let t = ad_hoc(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);

        let shown = describe(&t, at()).per_kwh().unwrap();
        let charged = rate(
            &t,
            &Chargeable::energy_only(
                Energy::from_kwh(dec("1")).unwrap(),
                at(),
                at() + time::Duration::minutes(30),
            )
            .unwrap(),
        )
        .lines[0]
            .unit_price;

        assert_eq!(shown, charged);
    }

    #[test]
    fn components_are_shown_in_the_prescribed_order() {
        // Written deliberately in the wrong order.
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Flat, dec("0.50")),
            PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
            PriceComponent::new(Dimension::Energy, dec("0.49")),
        ]);
        let shown = describe(&t, at());

        assert_eq!(
            shown.lines.iter().map(|l| l.dimension).collect::<Vec<_>>(),
            vec![Dimension::Energy, Dimension::ParkingTime, Dimension::Flat],
            "AFIR Art. 5(4) prescribes kWh, then minute, then session"
        );
        assert_eq!(
            shown.one_line(),
            "0.49 EUR / kWh · 0.10 EUR / min · 0.50 EUR / session"
        );
    }

    #[test]
    fn time_is_shown_per_minute_not_per_hour() {
        let t = ad_hoc(vec![PriceComponent::new(
            Dimension::ParkingTime,
            dec("6.00"),
        )]);
        let shown = describe(&t, at());
        assert_eq!(shown.occupancy_per_minute(), Some(dec("0.10")));
        assert_eq!(shown.lines[0].unit(), "min");
    }

    #[test]
    fn a_time_varying_tariff_discloses_both_sides_rather_than_only_now() {
        let night = TariffElement {
            components: vec![PriceComponent::new(Dimension::Energy, dec("0.29"))],
            restrictions: Restrictions {
                start_time: Some(time::macros::time!(22:00)),
                end_time: Some(time::macros::time!(06:00)),
                ..Restrictions::default()
            },
        };
        let t = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            tax_included: TaxIncluded::Yes,
            elements: vec![
                night,
                TariffElement::unrestricted(vec![PriceComponent::new(
                    Dimension::Energy,
                    dec("0.49"),
                )]),
            ],
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        };

        let at_noon = describe(&t, datetime!(2026-01-02 12:00 +1));
        assert_eq!(at_noon.per_kwh(), Some(dec("0.49")));
        assert!(at_noon.varies_by_condition());

        // A driver arriving at 21:58 can be shown *both* prices and when each
        // applies, rather than only the one that expires in two minutes.
        assert_eq!(
            at_noon.full_disclosure(),
            vec![
                "0.29 EUR / kWh (22:00–06:00)".to_owned(),
                "0.49 EUR / kWh".to_owned(),
            ]
        );

        let at_23 = describe(&t, datetime!(2026-01-02 23:00 +1));
        assert_eq!(at_23.per_kwh(), Some(dec("0.29")));
        assert!(at_23.tiers[0].applies_now);
        assert!(!at_23.tiers[1].applies_now);
    }

    #[test]
    fn a_tiered_tariff_discloses_its_tiers() {
        let t = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            tax_included: TaxIncluded::Yes,
            elements: vec![
                TariffElement {
                    components: vec![PriceComponent::new(Dimension::Energy, dec("0.39"))],
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

        let shown = describe(&t, at());
        assert_eq!(
            shown.per_kwh(),
            Some(dec("0.39")),
            "the first kilowatt-hour is billed at the first tier, so that is what is shown"
        );
        assert_eq!(
            shown.full_disclosure(),
            vec![
                "0.39 EUR / kWh (first 10 kWh)".to_owned(),
                "0.59 EUR / kWh".to_owned(),
            ]
        );
    }

    #[test]
    fn the_shown_tier_is_the_tier_the_first_kilowatt_hour_is_billed_at() {
        // The two must not be chosen by different rules, and the only way to
        // guarantee that is for them to call the same predicate.
        let t = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            tax_included: TaxIncluded::Yes,
            elements: vec![
                TariffElement {
                    components: vec![PriceComponent::new(Dimension::Energy, dec("0.39"))],
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
            Period::charging(
                at(),
                at() + time::Duration::minutes(15),
                Energy::from_kwh(dec("5")).unwrap(),
            ),
            Period::charging(
                at() + time::Duration::minutes(15),
                at() + time::Duration::minutes(30),
                Energy::from_kwh(dec("20")).unwrap(),
            ),
        ])
        .unwrap();

        assert_eq!(
            describe(&t, at()).per_kwh().unwrap(),
            rate(&t, &session).lines[0].unit_price
        );
    }

    #[test]
    fn the_description_carries_the_bounds() {
        let mut t = ad_hoc(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        t.min_price = Some(PriceLimit::gross(dec("2.00")));
        t.max_price = Some(PriceLimit::gross(dec("80.00")));
        let shown = describe(&t, at());
        assert_eq!(shown.min_price, Some(dec("2.00")));
        assert_eq!(shown.max_price, Some(dec("80.00")));
    }

    #[test]
    fn description_and_rating_agree_across_the_whole_component_set() {
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::Time, dec("3.60")),
            PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
            PriceComponent::new(Dimension::Flat, dec("0.50")),
        ]);
        let shown = describe(&t, at());

        let session = Chargeable::new(vec![
            Period::charging(
                at(),
                at() + time::Duration::hours(1),
                Energy::from_kwh(dec("1")).unwrap(),
            ),
            Period::parked(
                at() + time::Duration::hours(1),
                at() + time::Duration::hours(2),
            ),
        ])
        .unwrap();
        let rated = rate(&t, &session);

        for line in &rated.lines {
            let displayed = shown
                .line(line.dimension)
                .expect("every rated line is shown");
            let expected = price_in_display_unit(line.dimension, line.unit_price);
            assert_eq!(
                displayed.price, expected,
                "{:?} is shown at {} and rated at {}",
                line.dimension, displayed.price, line.unit_price
            );
        }
        assert_eq!(shown.lines.len(), rated.lines.len());
    }

    #[test]
    fn an_element_nobody_can_evaluate_never_shows_as_applying() {
        let t = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            tax_included: TaxIncluded::Yes,
            elements: vec![
                TariffElement {
                    components: vec![PriceComponent::new(Dimension::Energy, dec("0.19"))],
                    restrictions: Restrictions {
                        unevaluable: vec!["reservation=RESERVATION".to_owned()],
                        ..Restrictions::default()
                    },
                },
                TariffElement::unrestricted(vec![PriceComponent::new(
                    Dimension::Energy,
                    dec("0.49"),
                )]),
            ],
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        };

        let shown = describe(&t, at());
        assert_eq!(shown.per_kwh(), Some(dec("0.49")));
        assert!(!shown.tiers[0].applies_now);
        assert!(shown.tiers[0].condition.contains("unevaluable"));
    }

    #[test]
    fn a_line_renders_readably() {
        let line = DisplayLine {
            dimension: Dimension::Energy,
            price: dec("0.49"),
            vat: None,
        };
        assert_eq!(line.to_string(), "0.49 / kWh");
    }

    #[test]
    fn a_mixed_tariff_still_has_a_gesamtpreis() {
        // The reason the rate sits on the **line** and not on the description:
        // electricity at one rate beside a service fee at another has no single
        // rate to gross the display with, and it does have a Gesamtpreis —
        // one per component, which is exactly what `[PAngV §14(3)]` and
        // `[PAngV §3(3)]` ask for when a price is broken out.
        let mut tariff = Tariff::simple(
            "mixed".parse().unwrap(),
            Currency::EUR,
            crate::tariff::TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![
                crate::tariff::PriceComponent::new(Dimension::Energy, dec("0.49"))
                    .with_vat(dec("19")),
                crate::tariff::PriceComponent::new(Dimension::Flat, dec("1.00")).with_vat(dec("7")),
            ],
        );
        tariff.tax_included = crate::tariff::TaxIncluded::No;
        assert!(
            tariff.vat_basis().is_mixed(),
            "no single rate to fall back on"
        );

        let shown = describe_gross(&tariff, datetime!(2026-01-02 10:00 +1)).unwrap();
        assert!(shown.is_gesamtpreis());
        // Exact, and the scale is the product's own: 0.49 × 1.19 and
        // 1.00 × 1.07. Trailing zeros are an artefact of the multiplication
        // rather than a claim about precision, and trimming them here would be
        // this function deciding how a post renders a price.
        assert_eq!(shown.one_line(), "0.5831 EUR / kWh · 1.0700 EUR / session");
    }

    #[test]
    fn a_component_with_no_rate_is_refused_rather_than_grossed_by_zero() {
        // A rate nobody wrote down is not a rate of nothing. Grossing by zero
        // would show the net figure under a gross label, which is the one
        // outcome worse than refusing.
        let mut tariff = Tariff::simple(
            "silent".parse().unwrap(),
            Currency::EUR,
            crate::tariff::TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![crate::tariff::PriceComponent::new(
                Dimension::Energy,
                dec("0.49"),
            )],
        );
        tariff.tax_included = crate::tariff::TaxIncluded::No;

        assert_eq!(
            describe_gross(&tariff, datetime!(2026-01-02 10:00 +1)),
            Err(NoGesamtpreis::RateUnstated {
                dimension: Dimension::Energy
            })
        );
    }

    #[test]
    fn a_gross_bound_the_author_wrote_wins_over_one_derived_from_it() {
        // `[OCPI 2.3.0 §mod_tariffs_pricelimit_class]` gives a limit two limbs
        // that bind separately, so where the operator published the gross one
        // that is the figure a driver compares against — not the net one times
        // a rate this function chose.
        let mut tariff = Tariff::simple(
            "capped".parse().unwrap(),
            Currency::EUR,
            crate::tariff::TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![
                crate::tariff::PriceComponent::new(Dimension::Energy, dec("0.49"))
                    .with_vat(dec("19")),
            ],
        );
        tariff.tax_included = crate::tariff::TaxIncluded::No;
        tariff.max_price = Some(crate::tariff::PriceLimit {
            before_taxes: Some(dec("40.00")),
            after_taxes: Some(dec("47.50")),
        });

        let shown = describe_gross(&tariff, datetime!(2026-01-02 10:00 +1)).unwrap();
        assert_eq!(
            shown.max_price,
            Some(dec("47.50")),
            "the published gross ceiling, not 40.00 × 1.19"
        );

        // With only the net limb written down, the tariff's own single rate
        // grosses it.
        tariff.max_price = Some(crate::tariff::PriceLimit {
            before_taxes: Some(dec("40.00")),
            after_taxes: None,
        });
        let derived = describe_gross(&tariff, datetime!(2026-01-02 10:00 +1)).unwrap();
        assert_eq!(derived.max_price, Some(dec("47.60")));
    }
}
