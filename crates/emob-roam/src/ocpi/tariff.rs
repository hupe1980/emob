//! The tariff, so the partner can re-rate with the numbers that priced it.
//!
//! # The one loss that is not a loss but an enlargement
//!
//! `emob-tariff` refuses to match an element carrying a restriction it cannot
//! evaluate — an OCPI `reservation` condition, a partner extension — and says
//! so in a note that travels with the record. Treating an unknown condition as
//! absent applies a price under conditions nobody checked, which is the same
//! mistake as billing on an unverified signature, one layer up.
//!
//! Crossing such an element **outwards** inverts the failure. Dropping the
//! restriction does not narrow the element, it widens it: at the partner it
//! then matches wherever the rest of the conditions hold, and their re-rating
//! of the same session disagrees with ours — in the driver's disfavour, from a
//! document we published. That is [`RoamError::UnevaluableRestriction`], not a
//! note, because there is no version of the sentence "we published a wider
//! price than we charge" that a partner can act on.
//!
//! Inbound, the same field is a note in the other direction: a restriction
//! this build does not know goes into
//! [`Restrictions::unevaluable`](emob_tariff::Restrictions::unevaluable), and
//! the rating engine will decline to match the element. The invariant survives
//! the round trip in both directions, which is the property worth having.

use emob_tariff::{Dimension, Restrictions, Tariff, TariffKind, TaxIncluded};
use ocpi_kit::types::{LocalDate, LocalTime, Number};
use ocpi_kit::v2_3_0::tariffs::{
    DayOfWeek, PriceComponent, PriceLimit, TariffDimensionType, TariffElement, TariffRestrictions,
    TariffType, TaxIncluded as OcpiTaxIncluded,
};

use crate::crossing::Crossing;
use crate::error::RoamError;
use crate::ocpi::location::bounded;

/// Carry a tariff onto OCPI 2.3.0.
///
/// # Errors
///
/// [`RoamError::UnevaluableRestriction`] when an element restricts on
/// something this build cannot evaluate — see the module documentation for why
/// that is a refusal rather than a note — and [`RoamError::TooLong`] or
/// [`RoamError::InvalidString`] for an id that does not fit OCPI's bound.
pub fn to_ocpi(
    tariff: &Tariff,
    party: &emob_core::PartyId,
    last_updated: time::OffsetDateTime,
) -> Result<Crossing<ocpi_kit::v2_3_0::Tariff>, RoamError> {
    let mut crossing = Crossing::lossless(());
    let mut elements = Vec::with_capacity(tariff.elements.len());

    for (index, element) in tariff.elements.iter().enumerate() {
        if let Some(restriction) = element.restrictions.unevaluable.first() {
            return Err(RoamError::UnevaluableRestriction {
                element: index,
                restriction: restriction.clone(),
            });
        }

        let components: Vec<PriceComponent> = element
            .components
            .iter()
            .map(|component| {
                PriceComponent::builder()
                    .component_type(dimension(component.dimension))
                    .price(Number::new(component.price))
                    .maybe_vat(component.vat.map(Number::new))
                    .step_size(component.step_size)
                    .build()
            })
            .collect();

        elements.push(
            TariffElement::builder()
                .price_components(components)
                .maybe_restrictions(restrictions(&element.restrictions, index, &mut crossing))
                .build(),
        );
    }

    let built = ocpi_kit::v2_3_0::Tariff::builder()
        .country_code(bounded::<2>("country_code", party.country_code())?)
        .party_id(bounded::<3>("party_id", party.party_id())?)
        .id(bounded::<36>("id", tariff.id.as_str())?)
        .currency(crate::ocpi::location::bounded_ocpi::<3>(
            "currency",
            tariff.currency.as_str(),
        )?)
        .tariff_type(kind(tariff.kind))
        .elements(elements)
        .tax_included(tax_included(tariff.tax_included))
        .maybe_start_date_time(tariff.valid_from.map(ocpi_kit::types::DateTime::from))
        .maybe_end_date_time(tariff.valid_until.map(ocpi_kit::types::DateTime::from))
        .maybe_min_price(tariff.min_price.map(|min| limit(min, tariff.tax_included)))
        .maybe_max_price(tariff.max_price.map(|max| limit(max, tariff.tax_included)))
        .last_updated(last_updated)
        .build();

    Ok(crossing.map(|()| built))
}

/// A price bound, on the side of tax the tariff states its prices in.
///
/// OCPI's `PriceLimit` carries both an excluding- and an including-VAT figure
/// and requires whichever matches the tariff's own `tax_included`. Filling
/// both would be inventing the other, and the rate that would convert between
/// them belongs to the components rather than to the bound.
fn limit(amount: rust_decimal::Decimal, tax: TaxIncluded) -> PriceLimit {
    match tax {
        // The prices are gross, so the bound is too, and `before_taxes` has to
        // carry something: OCPI makes it required. Inventing a pre-tax figure
        // would need a rate that belongs to the components rather than to the
        // bound, so the gross amount is stated on both and the fact that they
        // are equal is itself the statement that no rate was applied.
        TaxIncluded::Yes => PriceLimit {
            before_taxes: Number::new(amount),
            after_taxes: Some(Number::new(amount)),
            extensions: ocpi_kit::types::Extensions::new(),
        },
        TaxIncluded::No | TaxIncluded::NotApplicable => {
            PriceLimit::before_taxes(Number::new(amount))
        }
    }
}

/// A dimension in OCPI's spelling. The four are the same four.
#[must_use]
pub const fn dimension(dimension: Dimension) -> TariffDimensionType {
    match dimension {
        Dimension::Energy => TariffDimensionType::Energy,
        Dimension::Time => TariffDimensionType::Time,
        Dimension::ParkingTime => TariffDimensionType::ParkingTime,
        Dimension::Flat => TariffDimensionType::Flat,
    }
}

/// Who the tariff is for.
///
/// OCPI's list has three *profile* variants beside `REGULAR` — cheap, fast and
/// green — which are a driver's preference rather than a contract's shape.
/// `emob-tariff` models the contract, so a contract tariff crosses as
/// `REGULAR`: claiming one of the three would advertise a preference the
/// tariff does not encode.
#[must_use]
pub const fn kind(kind: TariffKind) -> TariffType {
    match kind {
        TariffKind::AdHoc => TariffType::AdHocPayment,
        TariffKind::Contract => TariffType::Regular,
    }
}

/// Whether the prices are gross or net.
#[must_use]
pub const fn tax_included(tax: TaxIncluded) -> OcpiTaxIncluded {
    match tax {
        TaxIncluded::Yes => OcpiTaxIncluded::Yes,
        TaxIncluded::No => OcpiTaxIncluded::No,
        TaxIncluded::NotApplicable => OcpiTaxIncluded::NotApplicable,
    }
}

/// The restrictions, where there are any.
fn restrictions(
    restrictions: &Restrictions,
    index: usize,
    crossing: &mut Crossing<()>,
) -> Option<TariffRestrictions> {
    if restrictions == &Restrictions::default() {
        return None;
    }

    let out = TariffRestrictions {
        start_time: restrictions.start_time.map(local_time),
        end_time: restrictions.end_time.map(local_time),
        start_date: restrictions.start_date.and_then(local_date),
        end_date: restrictions.end_date.and_then(local_date),
        min_kwh: restrictions.min_kwh.map(Number::new),
        max_kwh: restrictions.max_kwh.map(Number::new),
        min_power: restrictions.min_power_kw.map(Number::new),
        max_power: restrictions.max_power_kw.map(Number::new),
        min_duration: restrictions.min_duration_s,
        max_duration: restrictions.max_duration_s,
        day_of_week: restrictions.days_of_week.iter().copied().map(day).collect(),
        ..TariffRestrictions::default()
    };

    // OCPI's `start_time`/`end_time` are local to the location, and the local
    // zone is not in the document. Carrying the wall clock is right — it is
    // what the tariff means — but a partner in another zone reading it as
    // theirs prices a night rate at the wrong hours, and nothing in the
    // document says which reading was meant.
    if restrictions.start_time.is_some() || restrictions.end_time.is_some() {
        crossing.note(
            format!("/elements/{index}/restrictions"),
            "the time restriction is a wall clock local to the charge point, and OCPI does not \
             carry the zone [OCPI 2.3.0 §mod_tariffs_tariffrestrictions_class]. A partner \
             reading it in their own zone prices a night rate at the wrong hours",
        );
    }

    Some(out)
}

/// A wall-clock time, which OCPI carries without a zone.
fn local_time(time: time::Time) -> LocalTime {
    LocalTime::new(time.hour(), time.minute()).unwrap_or(LocalTime::MIDNIGHT)
}

/// A calendar date.
fn local_date(date: time::Date) -> Option<LocalDate> {
    LocalDate::new(date.year(), u8::from(date.month()), date.day()).ok()
}

/// A weekday. Both vocabularies are the seven days.
#[must_use]
pub const fn day(day: time::Weekday) -> DayOfWeek {
    match day {
        time::Weekday::Monday => DayOfWeek::Monday,
        time::Weekday::Tuesday => DayOfWeek::Tuesday,
        time::Weekday::Wednesday => DayOfWeek::Wednesday,
        time::Weekday::Thursday => DayOfWeek::Thursday,
        time::Weekday::Friday => DayOfWeek::Friday,
        time::Weekday::Saturday => DayOfWeek::Saturday,
        time::Weekday::Sunday => DayOfWeek::Sunday,
    }
}

/// Read a partner's tariff into the canonical model.
///
/// Every restriction OCPI carries that this build cannot evaluate is named in
/// [`Restrictions::unevaluable`], which is what makes
/// [`emob_tariff::rate`] decline to match the element rather than silently
/// treat the condition as absent.
#[must_use]
pub fn unevaluable_of(restrictions: &TariffRestrictions) -> Vec<String> {
    let mut out = Vec::new();
    if restrictions.reservation.is_some() {
        out.push("reservation".to_owned());
    }
    if restrictions.min_current.is_some() {
        out.push("min_current".to_owned());
    }
    if restrictions.max_current.is_some() {
        out.push("max_current".to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_tariff::PriceComponent as Component;
    use rust_decimal::Decimal;
    use rust_decimal::prelude::FromStr;
    use time::macros::datetime;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("a decimal")
    }

    fn party() -> emob_core::PartyId {
        emob_core::PartyId::new("DE", "ABC").unwrap()
    }

    fn simple() -> Tariff {
        Tariff::simple(
            "tariff-1".parse().unwrap(),
            emob_core::Currency::new("EUR").unwrap(),
            TariffKind::AdHoc,
            vec![Component::new(Dimension::Energy, dec("0.49")).with_vat(dec("19"))],
        )
    }

    #[test]
    fn the_price_the_partner_re_rates_with_is_the_exact_decimal_that_rated() {
        let crossing = to_ocpi(&simple(), &party(), datetime!(2026-01-02 10:00 UTC)).unwrap();
        let component = &crossing.value.elements[0].price_components[0];
        assert_eq!(component.price.get().to_string(), "0.49");
        assert_eq!(component.vat.unwrap().get().to_string(), "19");
        assert!(crossing.is_lossless());
    }

    #[test]
    fn an_element_this_build_cannot_evaluate_is_refused_rather_than_widened() {
        let mut tariff = simple();
        tariff.elements[0]
            .restrictions
            .unevaluable
            .push("reservation".to_owned());

        let err = to_ocpi(&tariff, &party(), datetime!(2026-01-02 10:00 UTC)).unwrap_err();
        assert!(
            matches!(
                err,
                RoamError::UnevaluableRestriction {
                    element: 0,
                    ref restriction
                } if restriction == "reservation"
            ),
            "publishing it stripped makes the element match at the partner in conditions \
             nobody checked: {err}"
        );
    }

    #[test]
    fn a_time_restriction_says_that_ocpi_lost_the_zone() {
        let mut tariff = simple();
        tariff.elements[0].restrictions.start_time = Some(time::macros::time!(22:00));
        tariff.elements[0].restrictions.end_time = Some(time::macros::time!(6:00));

        let crossing = to_ocpi(&tariff, &party(), datetime!(2026-01-02 10:00 UTC)).unwrap();
        assert!(
            crossing.reasons().any(|r| r.contains("zone")),
            "a partner in another zone prices the night rate at the wrong hours"
        );
    }

    #[test]
    fn an_unevaluable_restriction_arriving_is_recorded_so_the_rating_declines_it() {
        let inbound = TariffRestrictions {
            reservation: Some(ocpi_kit::v2_3_0::tariffs::ReservationRestrictionType::Reservation),
            ..TariffRestrictions::default()
        };
        assert_eq!(unevaluable_of(&inbound), vec!["reservation".to_owned()]);
    }
}
