//! Public, gross EUR pass-through plans for the provider's pre-session disclosure
//! `[AFIR Art. 5(5)]`. A single composite tariff rates the retail example.

use emob_core::Currency;
use rust_decimal::Decimal;

use crate::{Dimension, PriceComponent, Tariff, TaxIncluded};

/// Explicit gross provider charges for a public plan. E-roaming is charged per session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fees {
    /// Gross amount per kWh.
    pub energy: Decimal,
    /// Gross amount per session.
    pub session: Decimal,
    /// Separate gross e-roaming fee per session.
    pub roaming: Decimal,
    /// Percentage of VAT included in each fee.
    pub vat: Decimal,
}

/// Combine retail rates before rating and rounding. Only simple unconditional,
/// single-step gross tariffs at the provider's tax rate are supported in v1.
///
/// # Errors
///
/// Returns the reason a public offer cannot represent the base tariff or fees.
pub fn public_retail_tariff(base: &Tariff, fees: Fees) -> Result<Tariff, &'static str> {
    if base.kind != crate::TariffKind::AdHoc {
        return Err("the operator base must be a public ad-hoc tariff");
    }
    if base.currency != Currency::EUR || base.tax_included != TaxIncluded::Yes {
        return Err("a public plan requires gross EUR prices");
    }
    if fees.vat < Decimal::ZERO
        || fees.vat > Decimal::from(100)
        || [fees.energy, fees.session, fees.roaming]
            .iter()
            .any(|n| *n < Decimal::ZERO || *n > Decimal::from(1_000_000))
    {
        return Err("invalid gross provider fee or VAT rate");
    }
    let [element] = base.elements.as_slice() else {
        return Err("multiple or missing tariff tiers");
    };
    if !element.restrictions.is_unrestricted()
        || base.min_price.is_some()
        || base.max_price.is_some()
    {
        return Err("conditional or bounded retail terms are unsupported");
    }
    for (i, component) in element.components.iter().enumerate() {
        if component.vat != Some(fees.vat)
            || component.price < Decimal::ZERO
            || component.price > Decimal::from(1_000_000)
            || component.step_size != 1
            || element.components[..i]
                .iter()
                .any(|c| c.dimension == component.dimension)
        {
            return Err("ambiguous VAT, steps or overlapping components");
        }
    }
    if element.component(Dimension::Energy).is_none() {
        return Err("the public plan needs an explicit base price per kWh");
    }
    let mut retail = base.clone();
    retail.kind = crate::TariffKind::Contract;
    let flat = fees
        .session
        .checked_add(fees.roaming)
        .ok_or("fee exceeds decimal range")?;
    for (dimension, extra) in [(Dimension::Energy, fees.energy), (Dimension::Flat, flat)] {
        if extra.is_zero() {
            continue;
        }
        let components = &mut retail.elements[0].components;
        if let Some(line) = components.iter_mut().find(|c| c.dimension == dimension) {
            line.price = line
                .price
                .checked_add(extra)
                .ok_or("retail price exceeds decimal range")?;
        } else {
            components.push(PriceComponent::new(dimension, extra).with_vat(fees.vat));
        }
    }
    Ok(retail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TariffKind, worked_total};
    use emob_core::TimeZone;
    use time::macros::datetime;

    #[test]
    fn public_fees_rate_10_kwh_to_six_euros_thirty_and_refuse_ambiguous_terms() {
        let vat = Decimal::from(19);
        let mut base = Tariff::simple(
            "ad-hoc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            TimeZone::utc(),
            vec![PriceComponent::new(Dimension::Energy, Decimal::new(50, 2)).with_vat(vat)],
        );
        let fees = Fees {
            energy: Decimal::new(1, 1),
            session: Decimal::ZERO,
            roaming: Decimal::new(3, 1),
            vat,
        };
        let retail = public_retail_tariff(&base, fees).unwrap();
        assert_eq!(
            worked_total(
                &retail,
                Decimal::TEN,
                Decimal::TEN,
                datetime!(2026-11-25 12:00 UTC)
            )
            .unwrap()
            .amount(),
            Decimal::new(630, 2)
        );
        base.kind = TariffKind::Contract;
        assert!(public_retail_tariff(&base, fees).is_err());
        base.kind = TariffKind::AdHoc;
        // SR-206. A VAT assumption or hidden billing step could change the driver's total.
        base.elements[0].components[0].step_size = 1000;
        assert!(public_retail_tariff(&base, fees).is_err());
        base.elements[0].components[0].step_size = 1;
        base.elements[0].components[0].vat = None;
        assert!(public_retail_tariff(&base, fees).is_err());
    }
}
