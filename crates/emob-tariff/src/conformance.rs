//! Whether a tariff is one a public charge point is allowed to charge.
//!
//! The obligation calendar in `emob-core` asks whether a *point* is compliant.
//! This asks the narrower question the calendar cannot: whether the *tariff*
//! itself satisfies `[AFIR Art. 5(4)]` at the power it will be charged at.
//!
//! The rule almost nothing checks:
//!
//! > At publicly accessible recharging points with a power output equal to or
//! > more than 50 kW, the ad hoc price charged by the operator **shall be based
//! > on the price per kWh** for the electricity delivered. In addition, the
//! > operators of those recharging points **can charge an occupancy fee as a
//! > price per minute** to discourage long occupancy.
//!
//! A per-minute-only tariff on a 150 kW charger is unlawful. So is one that
//! charges for charging *time* rather than for occupancy — the article permits
//! a fee for sitting there once charging has finished, not a rate for the
//! energy transfer dressed up as time. Both are ordinary commercial tariffs
//! elsewhere in the world and neither may be offered ad-hoc on a European fast
//! charger.

use rust_decimal::Decimal;

use crate::tariff::{Dimension, Tariff, TariffKind};

/// Something about a tariff that a regulator would object to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Objection {
    /// At 50 kW and above the ad-hoc price must be based on a price per kWh.
    NotEnergyBased,
    /// At 50 kW and above, time may only be charged as an occupancy fee — a
    /// price per minute for *not* charging — and not as a price for the
    /// charging time itself.
    ChargesForChargingTime,
    /// An element prices nothing at all.
    EmptyElement {
        /// Which element, by position.
        index: usize,
    },
    /// The tariff has no elements.
    NoElements,
    /// A component would round a quantity up against the customer.
    ///
    /// Lawful, and worth saying: OCPI 3.0 removes `step_size` and advises
    /// setting it to 1, so a tariff relying on it is one that will have to
    /// change.
    RoundsAgainstTheCustomer {
        /// Which dimension.
        dimension: Dimension,
        /// The block size.
        step_size: u32,
    },
}

impl Objection {
    /// Whether this makes the tariff unlawful, as opposed to merely awkward.
    #[must_use]
    pub const fn is_breach(&self) -> bool {
        match self {
            Self::NotEnergyBased
            | Self::ChargesForChargingTime
            | Self::EmptyElement { .. }
            | Self::NoElements => true,
            Self::RoundsAgainstTheCustomer { .. } => false,
        }
    }
}

impl core::fmt::Display for Objection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotEnergyBased => write!(
                f,
                "at 50 kW and above the ad-hoc price must be based on a price per kWh"
            ),
            Self::ChargesForChargingTime => write!(
                f,
                "at 50 kW and above, time may only be charged as an occupancy fee for not charging"
            ),
            Self::EmptyElement { index } => {
                write!(f, "tariff element {index} prices nothing")
            }
            Self::NoElements => write!(f, "the tariff has no elements"),
            Self::RoundsAgainstTheCustomer {
                dimension,
                step_size,
            } => write!(
                f,
                "{dimension:?} is billed in blocks of {step_size}, which rounds up against the customer (OCPI 3.0 removes step_size)"
            ),
        }
    }
}

/// Everything a regulator would object to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Conformance {
    /// The objections, most serious first.
    pub objections: Vec<Objection>,
}

impl Conformance {
    /// Whether the tariff may lawfully be offered.
    #[must_use]
    pub fn is_lawful(&self) -> bool {
        !self.objections.iter().any(Objection::is_breach)
    }

    /// The objections that make it unlawful.
    pub fn breaches(&self) -> impl Iterator<Item = &Objection> {
        self.objections.iter().filter(|o| o.is_breach())
    }

    /// One line per objection.
    pub fn reasons(&self) -> impl Iterator<Item = String> + '_ {
        self.objections.iter().map(ToString::to_string)
    }
}

/// Check a tariff against `[AFIR Art. 5(4)]` at the power it will be charged at.
///
/// ```
/// use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind, check_afir};
/// use emob_core::Currency;
/// use rust_decimal::Decimal;
/// use std::str::FromStr;
///
/// # let dec = |s: &str| Decimal::from_str(s).unwrap();
/// // A per-minute-only tariff: fine on a 22 kW post…
/// let by_the_minute = Tariff::simple(
///     "t".parse()?,
///     Currency::EUR,
///     TariffKind::AdHoc,
///     vec![PriceComponent::new(Dimension::Time, dec("0.10"))],
/// );
/// assert!(check_afir(&by_the_minute, dec("22")).is_lawful());
///
/// // …and unlawful on the 150 kW charger beside it.
/// assert!(!check_afir(&by_the_minute, dec("150")).is_lawful());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn check_afir(tariff: &Tariff, rated_power_kw: Decimal) -> Conformance {
    let mut objections = Vec::new();

    if tariff.elements.is_empty() {
        objections.push(Objection::NoElements);
    }
    for (index, element) in tariff.elements.iter().enumerate() {
        if element.components.is_empty() {
            objections.push(Objection::EmptyElement { index });
        }
    }

    // The article regulates the *ad-hoc* price. A contract tariff between a
    // provider and its own customer is governed by Art. 5(5) instead, which is
    // a disclosure duty rather than a shape duty.
    let is_fast = rated_power_kw >= Decimal::from(50);
    if tariff.kind == TariffKind::AdHoc && is_fast && !tariff.elements.is_empty() {
        if !tariff.prices_energy() {
            objections.push(Objection::NotEnergyBased);
        }
        if tariff.dimensions().contains(&Dimension::Time) {
            objections.push(Objection::ChargesForChargingTime);
        }
    }

    for element in &tariff.elements {
        for component in &element.components {
            if component.step_size > 1 {
                objections.push(Objection::RoundsAgainstTheCustomer {
                    dimension: component.dimension,
                    step_size: component.step_size,
                });
            }
        }
    }

    objections.sort_by_key(|o| !o.is_breach());
    Conformance { objections }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tariff::{PriceComponent, TariffElement, TaxIncluded};
    use emob_core::Currency;
    use rust_decimal::prelude::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
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
    fn a_kwh_tariff_is_lawful_at_any_power() {
        let t = ad_hoc(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        assert!(check_afir(&t, dec("22")).is_lawful());
        assert!(check_afir(&t, dec("350")).is_lawful());
    }

    #[test]
    fn a_per_minute_tariff_is_lawful_slow_and_unlawful_fast() {
        // The same tariff on two posts in one car park.
        let t = ad_hoc(vec![PriceComponent::new(Dimension::Time, dec("0.10"))]);
        assert!(check_afir(&t, dec("22")).is_lawful());

        let fast = check_afir(&t, dec("150"));
        assert!(!fast.is_lawful());
        assert!(fast.objections.contains(&Objection::NotEnergyBased));
        assert!(fast.reasons().any(|r| r.contains("price per kWh")));
    }

    #[test]
    fn the_fifty_kilowatt_boundary_is_inclusive() {
        let t = ad_hoc(vec![PriceComponent::new(Dimension::Time, dec("0.10"))]);
        assert!(check_afir(&t, dec("49.999")).is_lawful());
        assert!(!check_afir(&t, dec("50")).is_lawful());
    }

    #[test]
    fn an_occupancy_fee_on_top_of_kwh_is_exactly_what_the_article_permits() {
        // "In addition, the operators of those recharging points can charge an
        // occupancy fee as a price per minute."
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
        ]);
        assert!(check_afir(&t, dec("150")).is_lawful());
    }

    #[test]
    fn charging_time_dressed_up_beside_a_kwh_price_is_still_refused() {
        // Energy-based, so it passes the first test — but the extra charge is
        // for the *transfer*, not for occupancy, which the article does not
        // permit at 50 kW and above.
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::Time, dec("3.60")),
        ]);
        let c = check_afir(&t, dec("150"));
        assert!(!c.is_lawful());
        assert!(c.objections.contains(&Objection::ChargesForChargingTime));

        // Below 50 kW the article does not constrain the shape, so it stands.
        assert!(check_afir(&t, dec("22")).is_lawful());
    }

    #[test]
    fn a_contract_tariff_is_not_judged_by_the_ad_hoc_rule() {
        // Art. 5(4) regulates the ad-hoc price. A provider's own contract price
        // is governed by Art. 5(5), a disclosure duty rather than a shape one.
        let t = Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::Contract,
            vec![PriceComponent::new(Dimension::Time, dec("0.10"))],
        );
        assert!(check_afir(&t, dec("150")).is_lawful());
    }

    #[test]
    fn block_rounding_is_noted_without_being_a_breach() {
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_step_size(1000),
        ]);
        let c = check_afir(&t, dec("150"));
        assert!(c.is_lawful(), "lawful, and worth knowing about");
        assert!(
            c.objections
                .iter()
                .any(|o| matches!(o, Objection::RoundsAgainstTheCustomer { .. }))
        );
        assert!(c.reasons().any(|r| r.contains("OCPI 3.0")));
    }

    #[test]
    fn breaches_are_listed_before_notes() {
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Time, dec("0.10")).with_step_size(60),
        ]);
        let c = check_afir(&t, dec("150"));
        assert!(c.objections[0].is_breach(), "{:?}", c.objections);
        assert_eq!(c.breaches().count(), 2);
    }

    #[test]
    fn an_empty_tariff_is_refused() {
        let t = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            tax_included: TaxIncluded::Yes,
            elements: vec![],
            min_price: None,
            max_price: None,
        };
        assert!(!check_afir(&t, dec("22")).is_lawful());
        assert!(
            check_afir(&t, dec("22"))
                .objections
                .contains(&Objection::NoElements)
        );
    }

    #[test]
    fn an_element_that_prices_nothing_is_refused() {
        let t = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            tax_included: TaxIncluded::Yes,
            elements: vec![TariffElement::unrestricted(vec![])],
            min_price: None,
            max_price: None,
        };
        assert!(!check_afir(&t, dec("22")).is_lawful());
    }
}
