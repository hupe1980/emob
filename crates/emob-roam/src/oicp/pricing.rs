//! The canonical tariff onto an OICP pricing product — the only place a price
//! crosses this wire.
//!
//! # A product is not a tariff
//!
//! `[OICP 2.3 §PricingProductData]` gives a product **one** base price, charged
//! per one [`ReferenceUnit`] — an hour, a minute or a kilowatt-hour — plus a
//! short list of named extras: a start fee, a fixed fee, a parking fee, a floor
//! and a ceiling. There are no tiers, no per-dimension elements, and no tax.
//!
//! An `emob_tariff::Tariff` is a list of elements, each with restrictions and
//! several price components, and the rating engine chooses a price **per
//! dimension** at every instant. Most of that has no home here, so this crossing
//! refuses more than the OCPI one does — and refuses rather than approximates,
//! because a product that prices a session differently from the tariff that
//! rates it is the drift the whole workspace is arranged to prevent, arriving
//! through a translation instead of through a second engine.

use emob_tariff::{Dimension, Restrictions, Tariff, TariffElement, TaxIncluded};
use oicp_kit::cpo::{
    AdditionalReference, AdditionalReferenceType, PricingProductDataRecord, ProductAvailabilityTime,
};
use oicp_kit::types::{
    DaySelection, HourMinute, Number, Period, ReferenceUnit, Text, Validate as _,
};
use rust_decimal::Decimal;

use crate::crossing::Crossing;
use crate::error::RoamError;

/// The elements that price a **charging session** — everything a pricing product
/// is about.
///
/// `[OICP 2.3 §PricingProductDataRecord]` has no reservation. Its base price is
/// charged per hour, minute or kilowatt-hour of *recharging*, and
/// `AdditionalReferenceType` names a start fee, a fixed fee, a parking fee, a
/// floor and a ceiling — nothing for holding a post for a driver who has not
/// arrived.
///
/// Reading a reservation element as a candidate is wrong in both directions: a
/// tariff pricing energy beside a hold rate reads as one pricing energy **and**
/// charging time, which a product cannot state and which refuses a lawful
/// tariff; and a tariff pricing only a hold publishes that rate as the price per
/// hour of *charging* (D250). The same filter `check_afir` and
/// `emob_poi::rate::publish` apply, for the same reason: a reservation is not
/// recharging.
fn session_elements(tariff: &Tariff) -> impl Iterator<Item = (usize, &TariffElement)> {
    tariff
        .elements
        .iter()
        .enumerate()
        .filter(|(_, element)| element.restrictions.reservation.is_none())
}

/// The reservation elements a product cannot carry, as a note apiece.
///
/// Named rather than dropped: the rule for every crossing in this crate is that
/// a price which does not reach a document is a fact the sender has to be able
/// to see, and a provider settling against this product is settling against a
/// tariff with one more term in it.
fn note_reservations(tariff: &Tariff, crossing: &mut Crossing<()>) {
    for (index, element) in tariff.elements.iter().enumerate() {
        if let Some(outcome) = element.restrictions.reservation {
            crossing.note(
                format!("/elements/{index}"),
                format!(
                    "{} price component(s) price a {} and do not cross: a pricing product \
                     states what recharging costs per hour, minute or kilowatt-hour, and OICP \
                     has no reservation fee `[OICP 2.3 §PricingProductDataRecord]`. It stays a \
                     term of the framework agreement",
                    element.components.len(),
                    outcome.as_str()
                ),
            );
        }
    }
}

/// Carry a tariff onto the pricing product a provider re-derives a price from.
///
/// `max_power_kw` is the ceiling the product covers — `MaximumProductChargingPower`
/// — and it is an argument because it is a fact about the **estate** the product
/// is offered on rather than about the tariff. A tariff does not know which
/// posts it hangs on.
///
/// # Errors
///
/// [`RoamError::RestrictionNotExpressible`] where an element restricts on
/// something OICP cannot state — an energy or duration tier above all, because a
/// product has one base price and no way to say "the first four kilowatt-hours";
/// [`RoamError::NoRateForPriceLimit`] where two dimensions would both have to be
/// the base price; [`RoamError::UnevaluableRestriction`] where an element
/// carries a restriction this build does not understand; and
/// [`RoamError::NotConformant`] where the finished product fails the kit's own
/// schema.
pub fn to_oicp_product(
    tariff: &Tariff,
    product_id: &str,
    max_power_kw: Decimal,
) -> Result<Crossing<PricingProductDataRecord>, RoamError> {
    let mut crossing = Crossing::lossless(());

    // ── What the elements are allowed to say ────────────────────────────────
    for (index, element) in session_elements(tariff) {
        if !element.restrictions.is_evaluable() {
            return Err(RoamError::UnevaluableRestriction {
                element: index,
                restriction: element.restrictions.unevaluable.join(", "),
            });
        }
        refuse_untranslatable(index, &element.restrictions)?;
    }
    note_reservations(tariff, &mut crossing);

    // ── The base price: one dimension, one unit ─────────────────────────────
    let energy = price_of(tariff, Dimension::Energy);
    let time = price_of(tariff, Dimension::Time);
    let (reference_unit, base) = match (energy, time) {
        (Some(_), Some(_)) => {
            // `AdditionalReferenceType` has a start fee, a fixed fee, a parking
            // fee, a floor and a ceiling — and nothing for "energy" or "charging
            // time". So the second of the two has nowhere to go, and a product
            // carrying only the first prices a session the tariff does not.
            return Err(RoamError::NoRateForPriceLimit {
                field: "PricePerReferenceUnit".to_owned(),
            });
        }
        (Some(price), None) => (ReferenceUnit::KilowattHour, price),
        (None, Some(price)) => (ReferenceUnit::Hour, price),
        // A session fee alone. The kit states the specification's own rule: a
        // product whose price is a fixed fee should price the reference unit at
        // zero.
        (None, None) => (ReferenceUnit::Hour, Decimal::ZERO),
    };

    let extras = extras(
        tariff,
        &reference_unit,
        energy.is_none() && time.is_none(),
        &mut crossing,
    );

    // ── The tax, which does not cross at all ────────────────────────────────
    crossing.note(
        "/PricePerReferenceUnit",
        format!(
            "this price is stated {} and OICP carries no tax flag and no rate \
             `[OICP 2.3 §PricingProductDataRecord]`. Which of the two it is, is a term of the \
             framework agreement rather than of this document, and a provider that reads it the \
             other way is out by the whole VAT rate",
            match tariff.tax_included {
                TaxIncluded::Yes => "**gross**",
                TaxIncluded::No => "**net**",
                TaxIncluded::NotApplicable => "outside a tax regime",
            }
        ),
    );

    // ── When it applies ─────────────────────────────────────────────────────
    let (times, always) = availability(tariff, &mut crossing)?;

    let record = PricingProductDataRecord::builder()
        .product_id(text::<50>(product_id, "ProductID")?)
        .reference_unit(reference_unit.clone())
        .product_price_currency(text::<3>(&currency_of(tariff), "ProductPriceCurrency")?)
        .price_per_reference_unit(Number::new(base))
        .maximum_product_charging_power(Number::new(max_power_kw))
        .is_valid_24hours(always)
        .product_availability_times(times)
        .maybe_additional_references((!extras.is_empty()).then_some(extras))
        .build_unchecked();

    if let Err(violations) = record.validate() {
        return Err(RoamError::NotConformant {
            violations: violations
                .as_slice()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        });
    }
    Ok(crossing.map(|()| record))
}

/// The fees OICP has names for, and the two notes they cost.
///
/// `fixed` says the product's whole price is the fee, which is the
/// specification's own condition for pricing the reference unit at zero.
fn extras(
    tariff: &Tariff,
    reference_unit: &ReferenceUnit,
    fixed: bool,
    crossing: &mut Crossing<()>,
) -> Vec<AdditionalReference> {
    let mut extras = Vec::new();
    if let Some(price) = price_of(tariff, Dimension::Flat) {
        extras.push(fee(
            if fixed {
                AdditionalReferenceType::FixedFee
            } else {
                AdditionalReferenceType::StartFee
            },
            ReferenceUnit::Hour,
            price,
        ));
        crossing.note(
            "/AdditionalReferences",
            "a session fee is charged once and OICP requires every additional reference to name a \
             unit it is charged *per* `[OICP 2.3 §AdditionalReference]`. `HOUR` is written because \
             the field cannot be omitted, and a reader that divides by it gets a price per hour \
             nobody charges",
        );
    }
    if let Some(price) = price_of(tariff, Dimension::ParkingTime) {
        extras.push(fee(
            AdditionalReferenceType::ParkingFee,
            ReferenceUnit::Hour,
            price,
        ));
    }
    // OICP has one figure per bound where `[OCPI 2.3.0
    // §mod_tariffs_pricelimit_class]` has two, so what crosses is the bound in
    // the tariff's own basis — the figure a partner compares a total against.
    // The other limb has nowhere to go on this wire and stays behind.
    for (bound, kind) in [
        (
            tariff
                .min_price
                .and_then(|p| p.in_basis(tariff.tax_included)),
            AdditionalReferenceType::MinimumFee,
        ),
        (
            tariff
                .max_price
                .and_then(|p| p.in_basis(tariff.tax_included)),
            AdditionalReferenceType::MaximumFee,
        ),
    ] {
        if let Some(amount) = bound {
            extras.push(fee(kind.clone(), reference_unit.clone(), amount));
            crossing.note(
                "/AdditionalReferences",
                format!(
                    "a {} bounds the session **total** and OICP states it per reference unit, so \
                     it is written against `{}` — the same unit the base price uses, because there \
                     is no unit for \"the whole session\"",
                    kind.as_str(),
                    reference_unit.as_str()
                ),
            );
        }
    }
    extras
}

/// Refuse a restriction OICP has no way to state.
///
/// The energy and duration limbs are the important ones: they are what makes a
/// tariff *tiered*, and a product has one base price. Silently flattening a
/// tiered tariff onto its first band is a price the estate does not charge, in a
/// document a provider settles against.
fn refuse_untranslatable(index: usize, r: &Restrictions) -> Result<(), RoamError> {
    let lost: Option<(&'static str, &'static str)> = if r.min_kwh.is_some() || r.max_kwh.is_some() {
        Some((
            "min_kwh/max_kwh",
            "a pricing product has one base price and no way to say \"the first four \
             kilowatt-hours\": an energy tier has no spelling in OICP",
        ))
    } else if r.min_duration_s.is_some() || r.max_duration_s.is_some() {
        Some((
            "min_duration/max_duration",
            "a pricing product has one base price and no way to band it by elapsed time",
        ))
    } else if r.start_date.is_some() || r.end_date.is_some() {
        Some((
            "start_date/end_date",
            "`ProductAvailabilityTimes` states hours and weekdays and has no calendar: a product \
             that applies for one week cannot say so",
        ))
    } else {
        None
    };
    match lost {
        None => Ok(()),
        Some((field, detail)) => Err(RoamError::RestrictionNotExpressible {
            element: index,
            field,
            detail: detail.to_owned(),
        }),
    }
}

/// When the product applies, and whether that is simply "always".
///
/// OICP states availability as windows on a **single** [`DaySelection`], which
/// names every day, the working week, the weekend or one weekday. A tariff whose
/// element restricts to an arbitrary set — Monday and Thursday — has no
/// spelling here, and gets a refusal rather than a wider product.
fn availability(
    tariff: &Tariff,
    crossing: &mut Crossing<()>,
) -> Result<(Vec<ProductAvailabilityTime>, bool), RoamError> {
    let mut times = Vec::new();
    // A product says when *recharging* is priced this way. A reservation
    // element's window says when a hold is priced, which is a different
    // question and would narrow the product to hours it does not apply in.
    for (index, element) in session_elements(tariff) {
        let r = &element.restrictions;
        if r.start_time.is_none() && r.end_time.is_none() && r.days_of_week.is_empty() {
            continue;
        }
        let on = days(&r.days_of_week).ok_or_else(|| RoamError::RestrictionNotExpressible {
            element: index,
            field: "days_of_week",
            detail: format!(
                "{:?}: OICP names every day, the working week, the weekend or one weekday, and \
                 nothing else",
                r.days_of_week
            ),
        })?;
        let begin = r.start_time.unwrap_or(time::Time::MIDNIGHT);
        let end = r.end_time.unwrap_or(time::Time::MIDNIGHT);
        times.push(
            ProductAvailabilityTime::builder()
                .periods(vec![
                    Period::builder()
                        .begin(hour_minute(begin))
                        .end(hour_minute(end))
                        .build_unchecked(),
                ])
                .on(on)
                .build_unchecked(),
        );
    }
    if times.is_empty() {
        return Ok((Vec::new(), true));
    }
    crossing.note(
        "/ProductAvailabilityTimes",
        "the wall-clock windows cross as local civil times with no zone beside them: OICP states \
         a product's availability in hours and minutes and says nothing about which clock \
         `[OICP 2.3 §ProductAvailabilityTime]`. This side reads them in the tariff's own IANA \
         zone, and a provider in another one will not",
    );
    Ok((times, false))
}

/// The one `DaySelection` a weekday set maps onto, when it maps onto one.
fn days(set: &[time::Weekday]) -> Option<DaySelection> {
    use time::Weekday::{Friday, Monday, Saturday, Sunday, Thursday, Tuesday, Wednesday};
    if set.is_empty() {
        return Some(DaySelection::Everyday);
    }
    let mut sorted: Vec<u8> = set
        .iter()
        .map(|day| day.number_days_from_monday())
        .collect();
    sorted.sort_unstable();
    sorted.dedup();
    match sorted.as_slice() {
        [0, 1, 2, 3, 4, 5, 6] => Some(DaySelection::Everyday),
        [0, 1, 2, 3, 4] => Some(DaySelection::Workdays),
        [5, 6] => Some(DaySelection::Weekend),
        [one] => Some(match one {
            0 => DaySelection::Monday,
            1 => DaySelection::Tuesday,
            2 => DaySelection::Wednesday,
            3 => DaySelection::Thursday,
            4 => DaySelection::Friday,
            5 => DaySelection::Saturday,
            _ => DaySelection::Sunday,
        }),
        _ => {
            let _ = (
                Monday, Tuesday, Wednesday, Thursday, Friday, Saturday, Sunday,
            );
            None
        }
    }
}

/// The first price any element states for a dimension.
///
/// A tariff with two bands for one dimension has already been refused, so
/// "first" is "only".
/// The first price a **session** element states for a dimension.
///
/// Reservation elements are stepped over — see [`session_elements`].
fn price_of(tariff: &Tariff, dimension: Dimension) -> Option<Decimal> {
    session_elements(tariff)
        .find_map(|(_, element)| element.component(dimension))
        .map(|component| component.price)
}

fn fee(kind: AdditionalReferenceType, unit: ReferenceUnit, price: Decimal) -> AdditionalReference {
    AdditionalReference::builder()
        .additional_reference(kind)
        .additional_reference_unit(unit)
        .price_per_additional_reference_unit(Number::new(price))
        .build_unchecked()
}

fn hour_minute(at: time::Time) -> HourMinute {
    HourMinute::new_unchecked(format!("{:02}:{:02}", at.hour(), at.minute()))
}

fn currency_of(tariff: &Tariff) -> String {
    tariff.currency.to_string()
}

fn text<const N: usize>(value: &str, field: &'static str) -> Result<Text<N>, RoamError> {
    Text::<N>::new(value).map_err(|_| RoamError::TooLong {
        field,
        len: value.len(),
        max: N,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Weekday::{Friday, Monday, Saturday, Sunday, Thursday, Tuesday, Wednesday};

    #[test]
    fn a_weekday_set_crosses_only_where_oicp_has_a_name_for_it() {
        // `DaySelection` is one value, not a set: every day, the working week,
        // the weekend, or one named day. A tariff restricted to Monday **and**
        // Thursday has no spelling, and widening it to `Everyday` would publish
        // a price the estate charges on two days as one it charges on seven.
        assert_eq!(days(&[]), Some(DaySelection::Everyday));
        assert_eq!(
            days(&[
                Monday, Tuesday, Wednesday, Thursday, Friday, Saturday, Sunday
            ]),
            Some(DaySelection::Everyday)
        );
        assert_eq!(
            days(&[Monday, Tuesday, Wednesday, Thursday, Friday]),
            Some(DaySelection::Workdays)
        );
        assert_eq!(days(&[Saturday, Sunday]), Some(DaySelection::Weekend));
        assert_eq!(days(&[Wednesday]), Some(DaySelection::Wednesday));

        // …and the order it was written in does not decide the answer.
        assert_eq!(days(&[Sunday, Saturday]), Some(DaySelection::Weekend));

        assert_eq!(days(&[Monday, Thursday]), None);
        assert_eq!(days(&[Monday, Tuesday]), None);
    }

    fn dec(s: &str) -> Decimal {
        <Decimal as core::str::FromStr>::from_str(s).unwrap()
    }

    /// Energy at 0.49, and a reservation held at 5.00 an hour.
    fn tariff_with_reservation() -> Tariff {
        use emob_tariff::{PriceComponent, ReservationRestriction, TariffElement, TariffKind};
        Tariff {
            id: "t".parse().unwrap(),
            currency: emob_core::Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            tax_included: TaxIncluded::No,
            elements: vec![
                TariffElement {
                    components: vec![PriceComponent::new(Dimension::Time, dec("5.00"))],
                    restrictions: Restrictions {
                        reservation: Some(ReservationRestriction::Reservation),
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
        }
    }

    #[test]
    fn a_reservation_rate_is_not_this_products_price() {
        // OICP has no reservation: `AdditionalReferenceType` names a start fee,
        // a fixed fee, a parking fee, a floor and a ceiling and nothing else.
        // Read as a session element, the hold rate makes this tariff look like
        // one that prices energy *and* charging time — which OICP refuses — so a
        // perfectly translatable tariff was blocked from the wire.
        let crossing = to_oicp_product(&tariff_with_reservation(), "P1", dec("150"))
            .expect("a tariff that prices energy crosses; the hold rate is not a session price");
        let record = crossing.notes().to_vec();
        let product = crossing.into_value_discarding_notes();

        assert_eq!(product.reference_unit, ReferenceUnit::KilowattHour);
        assert_eq!(product.price_per_reference_unit.get(), dec("0.49"));
        assert!(
            record
                .iter()
                .any(|note| note.reason.contains("reservation")),
            "the price that did not cross is named: {record:?}"
        );
    }

    #[test]
    fn a_tariff_that_only_prices_a_reservation_publishes_no_charging_price() {
        // The failure the other way round: with no session element at all, the
        // hold rate became the product's base price per hour, and a partner
        // re-derived the *charging* price from it.
        use emob_tariff::{PriceComponent, ReservationRestriction, TariffElement};
        let mut tariff = tariff_with_reservation();
        tariff.elements = vec![TariffElement {
            components: vec![PriceComponent::new(Dimension::Time, dec("5.00"))],
            restrictions: Restrictions {
                reservation: Some(ReservationRestriction::Reservation),
                ..Restrictions::default()
            },
        }];

        let product = to_oicp_product(&tariff, "P1", dec("150"))
            .expect("a product with no session price is the specification's fixed-fee shape")
            .into_value_discarding_notes();
        assert_eq!(
            product.price_per_reference_unit.get(),
            Decimal::ZERO,
            "a reservation rate is not a price per hour of charging"
        );
    }
}
