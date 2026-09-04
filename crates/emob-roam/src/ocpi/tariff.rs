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
//!
//! # …and the same argument for a restriction that will not *fit*
//!
//! OCPI's `LocalTime` and `LocalDate` are narrower than the types this
//! workspace restricts on, and a bound that does not fit has exactly two silent
//! outcomes: dropped, which widens the element at the partner, or replaced with
//! a default, which moves it. A `start_time` falling back to midnight publishes
//! a night tariff as an all-day one — a *different* price, from a document this
//! operator signed off. So it is [`RoamError::RestrictionNotExpressible`], for
//! the reason the unevaluable one is refused.
//!
//! # The zone the wall clock is read in travels beside the tariff, not in it
//!
//! `22:00` is local civil time at the charge point, and OCPI puts the zone that
//! is read in on the **Location** — `time_zone`, an IANA name, cardinality 1
//! `[OCPI 2.3.0 §mod_locations_location_object]` — rather than on the Tariff.
//! So this crossing cannot state it in the object it produces, and the
//! constraint is on what the operator publishes *beside* it: every Location a
//! tariff applies at has to carry the zone the tariff was written in.
//!
//! That is worth a note rather than a shrug, because it is the one fact a
//! partner needs to reproduce a price and the one this document structurally
//! cannot carry. [`to_ocpi`] names [`Tariff::time_zone`] by JSON Pointer, so an
//! operator has the value to check their Locations against and a partner
//! settling a disputed session has it in the account of the crossing.

use emob_tariff::{
    Dimension, ReservationRestriction, Restrictions, Tariff, TariffKind, TaxIncluded,
};
use ocpi_kit::types::{LocalDate, LocalTime, Number};
use ocpi_kit::v2_3_0::tariffs::{
    DayOfWeek, PriceComponent, PriceLimit, ReservationRestrictionType, TariffDimensionType,
    TariffElement, TariffRestrictions, TariffType, TaxIncluded as OcpiTaxIncluded,
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
                .maybe_restrictions(restrictions(&element.restrictions, index)?)
                .build(),
        );
    }

    // OCPI puts the zone a tariff's wall clock is read in on the **Location**,
    // where it is mandatory, and not on the Tariff — so this document carries
    // `22:00` and nothing that says which 22:00
    // `[OCPI 2.3.0 §mod_locations_location_object]`. That is not a gap a
    // sender can close inside this object; it is a constraint on what the
    // sender must publish beside it. Naming the zone here gives the operator
    // the value their Locations have to agree with, and gives a partner
    // reconciling a disputed session the one fact the tariff cannot state.
    if tariff
        .elements
        .iter()
        .any(|element| element.restrictions.reads_the_wall_clock())
    {
        crossing.note(
            "/elements",
            format!(
                "this tariff's time, date and weekday restrictions are read on the wall clock of \
                 {}. OCPI carries that zone on the Location's `time_zone` and not on the Tariff \
                 [OCPI 2.3.0 §mod_locations_location_object], so every Location this tariff \
                 applies at must publish it — a partner evaluating these restrictions in any \
                 other zone prices the night rate at the wrong hours",
                tariff.time_zone
            ),
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
        .maybe_min_price(
            tariff
                .min_price
                .map(|min| limit(min, tariff, "/min_price", &mut crossing))
                .transpose()?,
        )
        .maybe_max_price(
            tariff
                .max_price
                .map(|max| limit(max, tariff, "/max_price", &mut crossing))
                .transpose()?,
        )
        .last_updated(last_updated)
        .build();

    Ok(crossing.map(|()| built))
}

/// A price bound, in the two figures OCPI states one in.
///
/// # `before_taxes` means before taxes
///
/// OCPI's `PriceLimit` requires `before_taxes` and makes `after_taxes`
/// optional, and the requirement it expresses is a *net* one: "the total cost
/// of a Charging Session before taxes can never be lower than the value of the
/// `min_price`'s `before_taxes` field" `[OCPI 2.3.0 §Tariff]`. A gross bound
/// written into that field is a bound the partner will enforce nineteen per
/// cent too high, against the driver, out of a document we published — and no
/// note can repair a number a partner is entitled to read at face value.
///
/// So a gross tariff's bound is converted, at the rate the tariff's own
/// components carry — [`emob_tariff::Tariff::vat_basis`], the same question the
/// OCPP crossing asks and the same choice the rating engine makes when a
/// minimum charge lands on a session with no lines.
///
/// # A tariff that states no rate is not a tariff that cannot answer
///
/// The two are distinct answers, and collapsing them into one `None` means an ordinary
/// gross price list carrying a `min_price` and no VAT rate anywhere could not be
/// published to a partner at all — over a diagnostic that said its components
/// carried more than one rate, which was not true of it. A basis nobody stated
/// is zero per cent for the arithmetic, exactly as
/// [`emob_tariff::Rated::tax_summary`] already reads it, so `before_taxes`
/// equals the bound and there is nothing to note. Only [`VatBasis::Mixed`] has
/// no answer.
///
/// # Errors
///
/// [`RoamError::NoRateForPriceLimit`] when the tariff's prices are gross and
/// its components carry more than one VAT rate, so no single taxable amount
/// corresponds to the bound.
fn limit(
    bound: emob_tariff::PriceLimit,
    tariff: &Tariff,
    pointer: &str,
    crossing: &mut Crossing<()>,
) -> Result<PriceLimit, RoamError> {
    // Both limbs stated: nothing to derive, and they bind separately, so both
    // travel `[OCPI 2.3.0 §mod_tariffs_pricelimit_class]`.
    if let (Some(net), Some(gross)) = (bound.before_taxes, bound.after_taxes) {
        return Ok(PriceLimit {
            before_taxes: Number::new(net),
            after_taxes: Some(Number::new(gross)),
            extensions: ocpi_kit::types::Extensions::new(),
        });
    }
    let Some(amount) = bound.in_basis(tariff.tax_included) else {
        return Ok(PriceLimit::before_taxes(Number::new(
            rust_decimal::Decimal::ZERO,
        )));
    };
    // A bound stated only after taxes needs the mandatory `before_taxes`
    // derived, whichever basis the *prices* are in.
    let derive = tariff.tax_included == TaxIncluded::Yes || bound.before_taxes.is_none();
    match if derive {
        TaxIncluded::Yes
    } else {
        TaxIncluded::No
    } {
        TaxIncluded::Yes => {
            let rate = tariff
                .vat_basis()
                .rate()
                .ok_or_else(|| RoamError::NoRateForPriceLimit {
                    field: pointer.trim_start_matches('/').to_owned(),
                })?;
            let factor = rust_decimal::Decimal::ONE + rate / rust_decimal::Decimal::from(100);
            // A rate of exactly −100 % makes the factor zero and no net grosses
            // up to a non-zero amount at it — the same hole the rating engine
            // reports rather than dividing into. Its own variant, because the
            // fix is to correct the rate and not to find a second one.
            if factor.is_zero() {
                return Err(RoamError::ImpossibleVatRateForPriceLimit {
                    field: pointer.trim_start_matches('/').to_owned(),
                    rate,
                });
            }
            let net = emob_core::Money::new(amount / factor, tariff.currency)
                .round_to_minor_unit()
                .amount();
            if net * factor != amount {
                crossing.note(
                    format!("{pointer}/before_taxes"),
                    format!(
                        "this tariff's prices are gross, and OCPI states a bound before taxes: {amount} at {rate} % is {net} to the minor unit, which grosses back up to {}",
                        net * factor
                    ),
                );
            }
            Ok(PriceLimit {
                before_taxes: Number::new(net),
                after_taxes: Some(Number::new(amount)),
                extensions: ocpi_kit::types::Extensions::new(),
            })
        }
        // The prices are already net, so the bound is the field's own figure
        // and the gross one would need a rate to invent.
        TaxIncluded::No | TaxIncluded::NotApplicable => {
            Ok(PriceLimit::before_taxes(Number::new(amount)))
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
///
/// # Errors
///
/// [`RoamError::RestrictionNotExpressible`] when a bound does not fit the field
/// OCPI carries it in. Neither silent outcome is a translation: a dropped
/// restriction **widens** the element at the partner, and a defaulted one moves
/// it — a `start_time` falling back to midnight publishes a night tariff as an
/// all-day one.
fn restrictions(
    restrictions: &Restrictions,
    index: usize,
) -> Result<Option<TariffRestrictions>, RoamError> {
    if restrictions == &Restrictions::default() {
        return Ok(None);
    }

    let out = TariffRestrictions {
        start_time: local_time(restrictions.start_time, index, "start_time")?,
        end_time: local_time(restrictions.end_time, index, "end_time")?,
        start_date: local_date(restrictions.start_date, index, "start_date")?,
        end_date: local_date(restrictions.end_date, index, "end_date")?,
        min_kwh: restrictions.min_kwh.map(Number::new),
        max_kwh: restrictions.max_kwh.map(Number::new),
        min_power: restrictions.min_power_kw.map(Number::new),
        max_power: restrictions.max_power_kw.map(Number::new),
        min_duration: restrictions.min_duration_s,
        max_duration: restrictions.max_duration_s,
        day_of_week: restrictions.days_of_week.iter().copied().map(day).collect(),
        reservation: restrictions.reservation.map(reservation),
        ..TariffRestrictions::default()
    };

    Ok(Some(out))
}

/// A wall-clock time, which OCPI carries without a zone.
fn local_time(
    time: Option<time::Time>,
    element: usize,
    field: &'static str,
) -> Result<Option<LocalTime>, RoamError> {
    time.map(|t| {
        LocalTime::new(t.hour(), t.minute()).map_err(|error| RoamError::RestrictionNotExpressible {
            element,
            field,
            detail: error.to_string(),
        })
    })
    .transpose()
}

/// A calendar date.
fn local_date(
    date: Option<time::Date>,
    element: usize,
    field: &'static str,
) -> Result<Option<LocalDate>, RoamError> {
    date.map(|d| {
        LocalDate::new(d.year(), u8::from(d.month()), d.day()).map_err(|error| {
            RoamError::RestrictionNotExpressible {
                element,
                field,
                detail: error.to_string(),
            }
        })
    })
    .transpose()
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

/// A reservation restriction, in OCPI's spelling. Both vocabularies name the
/// same two outcomes.
#[must_use]
pub const fn reservation(kind: ReservationRestriction) -> ReservationRestrictionType {
    match kind {
        ReservationRestriction::Reservation => ReservationRestrictionType::Reservation,
        ReservationRestriction::ReservationExpires => {
            ReservationRestrictionType::ReservationExpires
        }
    }
}

/// The same, read back off a partner's document.
///
/// A `match` with no catch-all, deliberately. An outcome a later revision of
/// OCPI adds is one this build cannot price, and a `_ => Reservation` arm would
/// price it as an ordinary one — so the arm does not exist and the upgrade is a
/// compile error here rather than a silent reading.
#[must_use]
pub const fn reservation_from_ocpi(kind: &ReservationRestrictionType) -> ReservationRestriction {
    match kind {
        ReservationRestrictionType::Reservation => ReservationRestriction::Reservation,
        ReservationRestrictionType::ReservationExpires => {
            ReservationRestriction::ReservationExpires
        }
    }
}

/// Read a partner's tariff into the canonical model.
///
/// Every restriction OCPI carries that this build cannot evaluate is named in
/// [`Restrictions::unevaluable`], which is what makes
/// [`emob_tariff::rate`] decline to match the element rather than silently
/// treat the condition as absent.
///
/// `reservation` is **not** on that list any more: it is evaluated, by
/// [`emob_tariff::rate_reservation`], against a window the session does not
/// have. What is left is the pair OCPI states in amperes, which needs a current
/// series no CDR carries.
#[must_use]
pub fn unevaluable_of(restrictions: &TariffRestrictions) -> Vec<String> {
    let mut out = Vec::new();
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
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
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
    fn a_gross_bound_crosses_as_the_bound_before_taxes_ocpi_asks_for() {
        // OCPI's `min_price.before_taxes` constrains the session's cost
        // **before taxes**. A gross figure written there is a minimum the
        // partner enforces nineteen per cent too high, against the driver,
        // from a document this operator published.
        let mut tariff = simple();
        tariff.min_price = Some(emob_tariff::PriceLimit::gross(dec("5.95")));

        let crossing = to_ocpi(&tariff, &party(), datetime!(2026-01-02 10:00 UTC)).unwrap();
        let min = crossing.value.min_price.as_ref().unwrap();
        assert_eq!(min.before_taxes.get(), dec("5.00"), "5.95 gross at 19 %");
        assert_eq!(min.after_taxes.as_ref().unwrap().get(), dec("5.95"));
        assert!(
            crossing.is_lossless(),
            "5.00 grosses back up to exactly 5.95: {:?}",
            crossing.notes()
        );

        // …and where the minor unit cannot hold the conversion exactly, the
        // partner is told by how much their copy fails to round-trip.
        let mut awkward = simple();
        awkward.min_price = Some(emob_tariff::PriceLimit::gross(dec("5.00")));
        let crossing = to_ocpi(&awkward, &party(), datetime!(2026-01-02 10:00 UTC)).unwrap();
        assert_eq!(
            crossing
                .value
                .min_price
                .as_ref()
                .unwrap()
                .before_taxes
                .get(),
            dec("4.20")
        );
        assert!(
            crossing
                .reasons()
                .any(|r| r.contains("/min_price/before_taxes")),
            "{:?}",
            crossing.notes()
        );
    }

    #[test]
    fn a_gross_bound_on_a_tariff_mixing_vat_rates_is_refused() {
        // There is no single taxable amount the bound corresponds to, and OCPI
        // makes the pre-tax figure mandatory — so the honest answer is that the
        // tariff cannot state this bound, not a number a partner would enforce.
        let mut mixed = simple();
        mixed.elements[0]
            .components
            .push(Component::new(Dimension::Flat, dec("0.50")).with_vat(dec("7")));
        mixed.max_price = Some(emob_tariff::PriceLimit::gross(dec("40.00")));

        let err = to_ocpi(&mixed, &party(), datetime!(2026-01-02 10:00 UTC)).unwrap_err();
        assert!(
            matches!(err, RoamError::NoRateForPriceLimit { ref field } if field == "max_price"),
            "{err}"
        );
    }

    #[test]
    fn a_gross_bound_on_a_tariff_that_states_no_rate_at_all_is_published() {
        // The case the previous reading could not tell from the one above, and
        // refused with its diagnostic: an ordinary gross price list that names
        // no VAT rate anywhere. There is nothing to strip out, so the bound
        // before taxes *is* the bound — which is what `Rated::tax_summary`
        // already computes one layer down, reading an absent rate as zero.
        //
        // Refusing it stopped a lawful tariff reaching a partner at all, over a
        // message that said its components carried more than one rate.
        let mut unstated = Tariff::simple(
            "tariff-1".parse().unwrap(),
            emob_core::Currency::new("EUR").unwrap(),
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![Component::new(Dimension::Energy, dec("0.49"))],
        );
        unstated.min_price = Some(emob_tariff::PriceLimit::gross(dec("5.00")));

        let crossing = to_ocpi(&unstated, &party(), datetime!(2026-01-02 10:00 UTC)).unwrap();
        let min = crossing.value.min_price.as_ref().unwrap();
        assert_eq!(min.before_taxes.get(), dec("5.00"));
        assert_eq!(min.after_taxes.unwrap().get(), dec("5.00"));
        assert!(
            crossing.is_lossless(),
            "nothing was rounded, so there is nothing to say: {:?}",
            crossing.notes()
        );
    }

    #[test]
    fn a_gross_bound_at_minus_one_hundred_per_cent_names_the_rate_rather_than_a_second_one() {
        // A rate this hostile has no net that grosses up to the bound. Its own
        // variant, because "correct the rate" and "you have two rates" send an
        // operator to different places.
        let mut impossible = Tariff::simple(
            "tariff-1".parse().unwrap(),
            emob_core::Currency::new("EUR").unwrap(),
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            vec![Component::new(Dimension::Energy, dec("0.49")).with_vat(dec("-100"))],
        );
        impossible.max_price = Some(emob_tariff::PriceLimit::gross(dec("40.00")));

        let err = to_ocpi(&impossible, &party(), datetime!(2026-01-02 10:00 UTC)).unwrap_err();
        assert!(
            matches!(
                err,
                RoamError::ImpossibleVatRateForPriceLimit { ref field, rate }
                    if field == "max_price" && rate == dec("-100")
            ),
            "{err}"
        );
    }

    #[test]
    fn a_net_tariffs_bound_is_the_figure_it_already_states() {
        let mut net = simple();
        net.tax_included = TaxIncluded::No;
        net.min_price = Some(emob_tariff::PriceLimit::net(dec("5.00")));

        let crossing = to_ocpi(&net, &party(), datetime!(2026-01-02 10:00 UTC)).unwrap();
        let min = crossing.value.min_price.as_ref().unwrap();
        assert_eq!(min.before_taxes.get(), dec("5.00"));
        assert!(
            min.after_taxes.is_none(),
            "the gross figure would need a rate to invent"
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
    fn every_restriction_this_build_can_express_reaches_the_partner() {
        // The failure this guards is a *silent* one: a bound that does not fit
        // the field OCPI carries it in has two quiet outcomes and both are
        // wrong — dropped, the element widens at the partner; defaulted, it
        // moves. A `start_time` defaulting to midnight publishes a night tariff
        // as an all-day one.
        //
        // Both conversions refuse rather than repair, and neither is reachable
        // from a `time::Time` or a `time::Date` — so what is worth asserting is
        // the property rather than the error: every field set here arrives,
        // with the value it was set to.
        let mut tariff = simple();
        tariff.elements[0].restrictions = Restrictions {
            start_time: Some(time::macros::time!(22:00)),
            end_time: Some(time::macros::time!(6:00)),
            start_date: Some(time::macros::date!(2026 - 04 - 01)),
            end_date: Some(time::macros::date!(2027 - 01 - 01)),
            min_kwh: Some(dec("10")),
            max_kwh: Some(dec("50")),
            min_power_kw: Some(dec("11")),
            max_power_kw: Some(dec("150")),
            min_duration_s: Some(600),
            max_duration_s: Some(7200),
            days_of_week: vec![time::Weekday::Saturday, time::Weekday::Sunday],
            reservation: Some(ReservationRestriction::ReservationExpires),
            unevaluable: Vec::new(),
        };

        let crossing = to_ocpi(&tariff, &party(), datetime!(2026-01-02 10:00 UTC)).unwrap();
        let r = crossing.value.elements[0].restrictions.as_ref().unwrap();

        assert_eq!(r.start_time.unwrap().to_string(), "22:00");
        assert_eq!(r.end_time.unwrap().to_string(), "06:00");
        assert_eq!(r.start_date.unwrap().to_string(), "2026-04-01");
        assert_eq!(r.end_date.unwrap().to_string(), "2027-01-01");
        assert_eq!(r.min_kwh.unwrap().get(), dec("10"));
        assert_eq!(r.max_kwh.unwrap().get(), dec("50"));
        assert_eq!(r.min_power.unwrap().get(), dec("11"));
        assert_eq!(r.max_power.unwrap().get(), dec("150"));
        assert_eq!(r.min_duration, Some(600));
        assert_eq!(r.max_duration, Some(7200));
        assert_eq!(r.day_of_week, vec![DayOfWeek::Saturday, DayOfWeek::Sunday]);
        assert_eq!(
            r.reservation,
            Some(ReservationRestrictionType::ReservationExpires)
        );
        // …and the one thing the crossing genuinely cannot carry is said.
        assert!(crossing.reasons().any(|reason| reason.contains("zone")));
    }

    #[test]
    fn an_unevaluable_restriction_arriving_is_recorded_so_the_rating_declines_it() {
        // Amperes: OCPI states a current band and no CDR carries the series to
        // judge it against, so the element matches nothing rather than being
        // widened into one that always applies.
        let inbound = TariffRestrictions {
            min_current: Some(Number::new(dec("5"))),
            ..TariffRestrictions::default()
        };
        assert_eq!(unevaluable_of(&inbound), vec!["min_current".to_owned()]);
    }

    #[test]
    fn a_reservation_restriction_is_evaluated_rather_than_refused() {
        // It used to be on the unevaluable list, which made a whole class of
        // tariff `[OCPI 2.3.0]` defines unpublishable and unpriceable. It is
        // now `emob_tariff::rate_reservation`'s, and both spellings are one
        // spelling.
        for (ours, theirs) in [
            (
                ReservationRestriction::Reservation,
                ReservationRestrictionType::Reservation,
            ),
            (
                ReservationRestriction::ReservationExpires,
                ReservationRestrictionType::ReservationExpires,
            ),
        ] {
            assert_eq!(reservation(ours), theirs);
            assert_eq!(reservation_from_ocpi(&theirs), ours);
            assert!(
                unevaluable_of(&TariffRestrictions {
                    reservation: Some(theirs),
                    ..TariffRestrictions::default()
                })
                .is_empty()
            );
        }
    }
}
