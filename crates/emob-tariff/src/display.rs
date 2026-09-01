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
//! same element-selection function — [`crate::rating::element_matches`] — that
//! [`crate::rating::rate`] uses for a session's first period. There is no
//! second source and no parallel rule, so there is nothing to drift from.
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
    /// The minimum a session will cost, when the tariff sets one.
    pub min_price: Option<Decimal>,
    /// The maximum, when it sets one.
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
/// use emob_core::Currency;
/// use rust_decimal::Decimal;
/// use std::str::FromStr;
/// use time::macros::datetime;
///
/// # let dec = |s: &str| Decimal::from_str(s).unwrap();
/// let tariff = Tariff::simple(
///     "ad-hoc".parse()?,
///     Currency::EUR,
///     TariffKind::AdHoc,
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
    let opening = SessionState {
        energy_kwh: Decimal::ZERO,
        elapsed_seconds: 0,
        at,
        power_kw: None,
    };

    let applicable = tariff
        .elements
        .iter()
        .position(|e| crate::rating::element_matches(e, &opening));

    let tiers: Vec<Tier> = tariff
        .elements
        .iter()
        .enumerate()
        .map(|(index, element)| Tier {
            condition: element.restrictions.describe(),
            lines: lines_of(element),
            applies_now: applicable == Some(index),
        })
        .collect();

    let lines = applicable
        .and_then(|index| tiers.get(index))
        .map(|tier| tier.lines.clone())
        .unwrap_or_default();

    PriceDescription {
        lines,
        tiers,
        currency: tariff.currency,
        tax_included: tariff.tax_included,
        min_price: tariff.min_price,
        max_price: tariff.max_price,
    }
}

/// One element's prices, in the order the article prescribes.
fn lines_of(element: &TariffElement) -> Vec<DisplayLine> {
    let mut lines: Vec<DisplayLine> = element
        .components
        .iter()
        .map(|c| DisplayLine {
            dimension: c.dimension,
            price: price_in_display_unit(c.dimension, c.price),
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
        Dimension::Time | Dimension::ParkingTime => price / Decimal::from(60),
        Dimension::Energy | Dimension::Flat => price,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rating::{Chargeable, Period, rate};
    use crate::tariff::{PriceComponent, Restrictions, TariffElement, TariffKind, TaxIncluded};
    use emob_core::Energy;
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
        t.min_price = Some(dec("2.00"));
        t.max_price = Some(dec("80.00"));
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
        };
        assert_eq!(line.to_string(), "0.49 / kWh");
    }
}
