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
//!
//! **And neither may a per-session fee.** That one is easy to miss, because the
//! article does list "price per session" — in its *third* subparagraph, which
//! governs points **below** 50 kW. At 50 kW and above the operator "shall …
//! show the ad hoc price per kWh and any possible occupancy fee expressed in
//! price per minute", and a fee outside those two cannot be displayed. A
//! charge the driver could not have been shown before starting defeats the
//! comparison the whole article exists to enable.

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
    /// At 50 kW and above, a per-session fee is a component the article does
    /// not admit.
    ///
    /// The second subparagraph enumerates what must be shown at the station:
    /// "the ad hoc price per kWh **and any possible occupancy fee** expressed
    /// in price per minute". Two components, named. A per-session fee is not
    /// one of them, so it could not lawfully be displayed — and the third
    /// subparagraph, which *does* list "price per session", governs points
    /// **below** 50 kW. A charge that cannot be shown before the session
    /// defeats the comparison the article exists to enable.
    ChargesPerSession,
    /// An element prices nothing at all.
    EmptyElement {
        /// Which element, by position.
        index: usize,
    },
    /// The tariff has no elements.
    NoElements,
    /// The minimum a session costs is above the maximum, so no total satisfies
    /// both.
    MinimumAboveMaximum {
        /// The minimum.
        minimum: Decimal,
        /// The maximum.
        maximum: Decimal,
    },
    /// A time price is quoted per hour and cannot be written exactly as a price
    /// per minute.
    ///
    /// `[AFIR Art. 5(4)]` asks for a **price per minute** — in the second
    /// subparagraph for the occupancy fee at 50 kW and above, and in the third
    /// for the component list below it. OCPI carries time prices per hour, so
    /// the display divides by sixty, and sixty has a factor of three: an
    /// ordinary occupancy fee of `2.50` an hour is `0.041666…` a minute, which
    /// has no exact decimal spelling at all.
    ///
    /// A platform that rounds it shows a price it does not charge, which is the
    /// `[PAngV]` failure the display module exists to make unrepresentable; one that
    /// does not shows a driver twenty-eight digits. Neither is a price "known to
    /// end users before they initiate a recharging session", so the tariff has
    /// to be quoted differently — and an hourly rate divisible by three is.
    NotShowablePerMinute {
        /// Which time dimension.
        dimension: Dimension,
        /// The price per hour that has no exact per-minute spelling.
        per_hour: Decimal,
    },
    /// A component carries a VAT rate no net-and-tax split can be computed
    /// from.
    ///
    /// A gross amount is `net × (1 + rate/100)`, so at exactly −100 % the
    /// factor is zero and no net grosses up to it. An invoice under such a
    /// tariff would state a taxable amount it cannot justify `[UStG §14]`.
    ImpossibleVatRate {
        /// Which dimension carried it.
        dimension: Dimension,
        /// The rate.
        rate: Decimal,
    },
    /// An element carries a restriction this build cannot evaluate.
    ///
    /// For an ad-hoc tariff this is a breach rather than a nuisance: the price
    /// has to be known to the driver before the session `[AFIR Art. 5(4)]`, and
    /// a price whose conditions cannot be checked cannot be shown correctly.
    CannotBeEvaluated {
        /// Which element, by position.
        index: usize,
        /// The restrictions that could not be judged.
        restrictions: Vec<String>,
    },
    /// An element sits behind an unrestricted one and can therefore never
    /// apply.
    ///
    /// Lawful and almost always a mistake: the prices in it will never be
    /// charged, and somebody believes they will.
    UnreachableElement {
        /// Which element, by position.
        index: usize,
        /// The unrestricted element that shadows it.
        shadowed_by: usize,
    },
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
            | Self::ChargesPerSession
            | Self::EmptyElement { .. }
            | Self::NoElements
            | Self::MinimumAboveMaximum { .. }
            | Self::NotShowablePerMinute { .. }
            | Self::ImpossibleVatRate { .. }
            | Self::CannotBeEvaluated { .. } => true,
            Self::RoundsAgainstTheCustomer { .. } | Self::UnreachableElement { .. } => false,
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
            Self::ChargesPerSession => write!(
                f,
                "at 50 kW and above the station must show the price per kWh and any occupancy fee, and nothing else: a per-session fee cannot be displayed and so cannot be charged"
            ),
            Self::NotShowablePerMinute {
                dimension,
                per_hour,
            } => write!(
                f,
                "{dimension:?} is {per_hour} per hour, which has no exact price per minute ({per_hour} / 60 does not terminate): [AFIR Art. 5(4)] asks for a price per minute, and a rounded one is not the price charged. Quote an hourly rate divisible by three"
            ),
            Self::ImpossibleVatRate { dimension, rate } => write!(
                f,
                "{dimension:?} carries a VAT rate of {rate} %, from which no taxable amount can be computed: an invoice under this tariff could not state one [UStG §14]"
            ),
            Self::EmptyElement { index } => {
                write!(f, "tariff element {index} prices nothing")
            }
            Self::NoElements => write!(f, "the tariff has no elements"),
            Self::MinimumAboveMaximum { minimum, maximum } => write!(
                f,
                "the minimum {minimum} is above the maximum {maximum}: no session total satisfies both"
            ),
            Self::CannotBeEvaluated {
                index,
                restrictions,
            } => write!(
                f,
                "element {index} carries restrictions this build cannot evaluate, so its price cannot be shown before the session: {restrictions:?}"
            ),
            Self::UnreachableElement { index, shadowed_by } => write!(
                f,
                "element {index} sits behind the unrestricted element {shadowed_by} and can never apply"
            ),
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
/// // 6.00 an hour is 0.10 a minute, which is what the driver is shown.
/// let by_the_minute = Tariff::simple(
///     "t".parse()?,
///     Currency::EUR,
///     TariffKind::AdHoc,
///     vec![PriceComponent::new(Dimension::Time, dec("6.00"))],
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
    if let (Some(minimum), Some(maximum)) = (tariff.min_price, tariff.max_price)
        && minimum > maximum
    {
        objections.push(Objection::MinimumAboveMaximum { minimum, maximum });
    }

    let mut first_unrestricted: Option<usize> = None;
    for (index, element) in tariff.elements.iter().enumerate() {
        if element.components.is_empty() {
            objections.push(Objection::EmptyElement { index });
        }
        if !element.restrictions.is_evaluable() && tariff.kind == TariffKind::AdHoc {
            objections.push(Objection::CannotBeEvaluated {
                index,
                restrictions: element.restrictions.unevaluable.clone(),
            });
        }
        match first_unrestricted {
            Some(shadowed_by) => {
                objections.push(Objection::UnreachableElement { index, shadowed_by });
            }
            None if element.restrictions.is_unrestricted() => first_unrestricted = Some(index),
            None => {}
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
        // The article grants exactly one addition to the kWh price — "an
        // occupancy fee as a price per minute" — and the display duty beside
        // it names exactly two components. Everything else is a charge the
        // driver could not have been shown before starting.
        if tariff.dimensions().contains(&Dimension::Time) {
            objections.push(Objection::ChargesForChargingTime);
        }
        if tariff.dimensions().contains(&Dimension::Flat) {
            objections.push(Objection::ChargesPerSession);
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
            // The article asks for a price per **minute**; OCPI carries one per
            // hour. Sixty has a factor of three, so an ordinary 2.50 an hour has
            // no exact per-minute spelling — and only the ad-hoc price is one a
            // driver has to be shown before starting.
            if tariff.kind == TariffKind::AdHoc
                && matches!(
                    component.dimension,
                    Dimension::Time | Dimension::ParkingTime
                )
                && !divides_exactly_into_minutes(component.price)
            {
                objections.push(Objection::NotShowablePerMinute {
                    dimension: component.dimension,
                    per_hour: component.price,
                });
            }
            if let Some(rate) = component.vat
                && (Decimal::ONE + rate / Decimal::from(100)).is_zero()
            {
                objections.push(Objection::ImpossibleVatRate {
                    dimension: component.dimension,
                    rate,
                });
            }
        }
    }

    objections.sort_by_key(|o| !o.is_breach());
    Conformance { objections }
}

/// Whether an hourly price has an exact price per minute.
///
/// A decimal terminates only when its denominator's prime factors are two and
/// five; sixty carries a three, so `2.50 / 60` does not terminate and `0.36 / 60`
/// does. Rather than reasoning about factors, the division is done and undone:
/// a quotient the arithmetic had to truncate does not multiply back.
fn divides_exactly_into_minutes(per_hour: Decimal) -> bool {
    let per_minute = per_hour / Decimal::from(60);
    per_minute * Decimal::from(60) == per_hour
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
        let t = ad_hoc(vec![PriceComponent::new(Dimension::Time, dec("6.00"))]);
        assert!(check_afir(&t, dec("22")).is_lawful());

        let fast = check_afir(&t, dec("150"));
        assert!(!fast.is_lawful());
        assert!(fast.objections.contains(&Objection::NotEnergyBased));
        assert!(fast.reasons().any(|r| r.contains("price per kWh")));
    }

    #[test]
    fn the_fifty_kilowatt_boundary_is_inclusive() {
        let t = ad_hoc(vec![PriceComponent::new(Dimension::Time, dec("6.00"))]);
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
    fn a_session_fee_is_refused_on_a_fast_charger_and_permitted_below_it() {
        // The trap: the article *does* list "price per session" — in the
        // subparagraph that governs points below 50 kW. At 50 kW and above the
        // station must show the price per kWh and any occupancy fee, and a fee
        // outside those two could not lawfully be displayed.
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::Flat, dec("0.50")),
        ]);

        let fast = check_afir(&t, dec("150"));
        assert!(!fast.is_lawful());
        assert!(fast.objections.contains(&Objection::ChargesPerSession));
        assert!(fast.reasons().any(|r| r.contains("cannot be displayed")));

        // Below 50 kW the third subparagraph names it explicitly, so it
        // stands — and `describe` shows it in the prescribed order.
        assert!(check_afir(&t, dec("22")).is_lawful());
    }

    #[test]
    fn the_only_addition_a_fast_charger_may_make_is_the_occupancy_fee() {
        // The property behind the two refusals: at 50 kW and above the lawful
        // component set is exactly {kWh} or {kWh, occupancy}.
        for extra in [Dimension::Time, Dimension::Flat] {
            let t = ad_hoc(vec![
                PriceComponent::new(Dimension::Energy, dec("0.49")),
                PriceComponent::new(extra, dec("1.00")),
            ]);
            assert!(
                !check_afir(&t, dec("150")).is_lawful(),
                "{extra:?} is not an addition the article admits"
            );
        }
        let permitted = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
        ]);
        assert!(check_afir(&permitted, dec("150")).is_lawful());
    }

    #[test]
    fn an_hourly_price_with_no_exact_per_minute_spelling_cannot_be_shown() {
        // The article asks for a price per minute and OCPI carries one per
        // hour. Sixty has a factor of three, so an ordinary occupancy fee of
        // 2.50 an hour is 0.041666… a minute — and a driver at the station was
        // being shown twenty-eight digits of it.
        let awkward = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::ParkingTime, dec("2.50")),
        ]);
        let c = check_afir(&awkward, dec("150"));
        assert!(!c.is_lawful());
        assert!(
            c.objections
                .iter()
                .any(|o| matches!(o, Objection::NotShowablePerMinute { .. })),
            "{:?}",
            c.objections
        );
        assert!(c.reasons().any(|r| r.contains("divisible by three")));

        // 6.00 an hour is 0.10 a minute, and 0.36 an hour is 0.006 — both
        // exact, both showable.
        for exact in ["6.00", "0.36", "3.00", "0.60"] {
            let t = ad_hoc(vec![
                PriceComponent::new(Dimension::Energy, dec("0.49")),
                PriceComponent::new(Dimension::ParkingTime, dec(exact)),
            ]);
            assert!(check_afir(&t, dec("150")).is_lawful(), "{exact} per hour");
        }

        // A contract price is not shown at the point, so the display duty does
        // not reach it.
        let mut contract = awkward;
        contract.kind = TariffKind::Contract;
        assert!(check_afir(&contract, dec("22")).is_lawful());
    }

    #[test]
    fn a_vat_rate_that_admits_no_taxable_amount_is_refused() {
        // `net × (1 + rate/100)` is the gross, so at exactly −100 % there is no
        // net at all. An invoice under this tariff could not state a taxable
        // amount `[UStG §14]`, and the rating engine had to divide by zero to
        // find that out.
        let t = ad_hoc(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("-100")),
        ]);
        let c = check_afir(&t, dec("22"));
        assert!(!c.is_lawful());
        assert!(
            c.objections
                .iter()
                .any(|o| matches!(o, Objection::ImpossibleVatRate { .. })),
            "{:?}",
            c.objections
        );
    }

    #[test]
    fn a_contract_tariff_is_not_judged_by_the_ad_hoc_rule() {
        // Art. 5(4) regulates the ad-hoc price. A provider's own contract price
        // is governed by Art. 5(5), a disclosure duty rather than a shape one.
        let t = Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::Contract,
            vec![PriceComponent::new(Dimension::Time, dec("6.00"))],
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
            PriceComponent::new(Dimension::Time, dec("6.00")).with_step_size(60),
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
            valid_from: None,
            valid_until: None,
        };
        assert!(!check_afir(&t, dec("22")).is_lawful());
        assert!(
            check_afir(&t, dec("22"))
                .objections
                .contains(&Objection::NoElements)
        );
    }

    #[test]
    fn a_minimum_above_the_maximum_is_incoherent() {
        let mut t = ad_hoc(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        t.min_price = Some(dec("10.00"));
        t.max_price = Some(dec("5.00"));
        let c = check_afir(&t, dec("22"));
        assert!(!c.is_lawful());
        assert!(
            c.objections
                .iter()
                .any(|o| matches!(o, Objection::MinimumAboveMaximum { .. }))
        );
    }

    #[test]
    fn an_ad_hoc_price_that_cannot_be_evaluated_cannot_be_shown() {
        // AFIR Art. 5(4) asks for the price to be known before the session.
        // An element whose conditions this build cannot check is one whose
        // price it cannot display correctly.
        let t = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            tax_included: TaxIncluded::Yes,
            elements: vec![TariffElement {
                components: vec![PriceComponent::new(Dimension::Energy, dec("0.49"))],
                restrictions: crate::tariff::Restrictions {
                    unevaluable: vec!["reservation=RESERVATION".to_owned()],
                    ..crate::tariff::Restrictions::default()
                },
            }],
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        };
        assert!(!check_afir(&t, dec("22")).is_lawful());

        // A contract tariff is a different question: it is not displayed at the
        // point, so an unevaluable restriction is a roaming integration problem
        // rather than a breach of the article.
        let mut contract = t;
        contract.kind = TariffKind::Contract;
        assert!(check_afir(&contract, dec("22")).is_lawful());
    }

    #[test]
    fn an_element_behind_an_unrestricted_one_can_never_apply() {
        let t = Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            tax_included: TaxIncluded::Yes,
            elements: vec![
                TariffElement::unrestricted(vec![PriceComponent::new(
                    Dimension::Energy,
                    dec("0.49"),
                )]),
                TariffElement {
                    components: vec![PriceComponent::new(Dimension::Energy, dec("0.29"))],
                    restrictions: crate::tariff::Restrictions {
                        start_time: Some(time::macros::time!(22:00)),
                        ..crate::tariff::Restrictions::default()
                    },
                },
            ],
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        };
        let c = check_afir(&t, dec("22"));
        assert!(c.is_lawful(), "lawful, and almost certainly a mistake");
        assert!(
            c.objections.iter().any(|o| matches!(
                o,
                Objection::UnreachableElement {
                    index: 1,
                    shadowed_by: 0
                }
            )),
            "{:?}",
            c.objections
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
            valid_from: None,
            valid_until: None,
        };
        assert!(!check_afir(&t, dec("22")).is_lawful());
    }
}
