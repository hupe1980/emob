//! The price, carried from the tariff that rates the session into the feed.
//!
//! # Why this is not a formatting problem
//!
//! `[AFIR Art. 20(2)(c)]` makes the ad-hoc price dynamic data an operator must
//! publish through the national access point, and `[AFIR Art. 5(2)]` makes the
//! price a driver is shown before the session the price they may be charged.
//! Those are two duties about **one number**, and almost every stack in this
//! market computes it twice: once in the billing system that rates the CDR, and
//! once in the export job that fills the DATEX II feed.
//!
//! Two computations of one number is two chances to be wrong, and the failure
//! is asymmetric — the feed is read by route planners and comparison sites, and
//! nobody reconciles it against an invoice. So the price here is the
//! [`emob_tariff::Tariff`] itself, the same value the same crate charges with,
//! in exact decimal from end to end.
//!
//! # What the profile cannot say
//!
//! `[DATEX-II-Profil Tab. A.116]` offers six price types: `basePrice`,
//! `flatRate`, `free`, `other`, `pricePerKWh` and `pricePerMinute`. Two
//! consequences fall out, and both are reported rather than papered over:
//!
//! - **There is no per-hour price type.** `emob-tariff` prices time by the
//!   hour, because that is how a tariff is written and displayed. The
//!   conversion is a division by sixty, and an hourly price that does not
//!   divide into a terminating decimal cannot be stated exactly per minute.
//! - **There is no occupancy price type.** `[AFIR Art. 5(4)]` explicitly
//!   permits a fee per minute for time connected and *not* charging, at points
//!   of 50 kW and above. The profile's only hook for it is
//!   `EnergyRate.combinationWithParkingFee`, a **boolean** saying whether
//!   charging and parking are one fee — not what the parking costs. So the one
//!   surcharge the Regulation names cannot be published as a number under the
//!   profile the same Regulation requires.
//!
//! Neither is a reason to refuse to publish. Both are reasons to say so, which
//! is what [`RateNote`] is for.
//!
//! # What the profile *can* say, in whole units
//!
//! A tiered tariff — "the first 10 kWh at 0.39, the rest at 0.59" — publishes
//! two prices, and a price published without the condition it applies under
//! reads as unconditional. The profile has the two fields for it:
//! `EnergyPrice.energyBasedApplicability` (`fromKWh`, `toKWh`) and
//! `EnergyPrice.timeBasedApplicability` (`fromMinute`, `toMinute`), and its own
//! note that "all prices belonging to one rate are applied within their
//! applicability".
//!
//! Both are **non-negative integers** and a tariff's thresholds are not, and
//! the two roundings are not equally wrong: `fromKWh: 10` for a tier beginning
//! at 10.5 claims the price applies over `[10, 10.5)`, where it does not. A
//! lower bound rounds **up** and an upper bound rounds **down**, so the
//! published band is a subset of the real one and every statement in the
//! document is true. [`RateNote`] carries the figure that had to move.

use core::cmp::Ordering;

use emob_core::Currency;
use emob_tariff::{Dimension, PriceComponent, Tariff, TariffKind, TaxIncluded};
use rust_decimal::Decimal;

/// The profile's price types `[DATEX-II-Profil Tab. A.116]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PriceType {
    /// A base price, to be added to the others.
    BasePrice,
    /// A flat rate, independent of the amount delivered.
    FlatRate,
    /// Free.
    Free,
    /// Something the enumeration does not name.
    Other,
    /// Per kWh of electric energy.
    PricePerKwh,
    /// Per minute of charging.
    PricePerMinute,
}

impl PriceType {
    /// The spelling `[DATEX-II-Profil Tab. A.116]` uses.
    #[must_use]
    pub const fn as_profile_str(self) -> &'static str {
        match self {
            Self::BasePrice => "basePrice",
            Self::FlatRate => "flatRate",
            Self::Free => "free",
            Self::Other => "other",
            Self::PricePerKwh => "pricePerKWh",
            Self::PricePerMinute => "pricePerMinute",
        }
    }
}

/// Ad hoc, or under a contract `[DATEX-II-Profil Tab. A.118]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RatePolicy {
    /// The price a driver with no contract pays.
    AdHoc,
    /// The price under a contract.
    Contract,
}

impl RatePolicy {
    /// The spelling the profile uses.
    #[must_use]
    pub const fn as_profile_str(self) -> &'static str {
        match self {
            Self::AdHoc => "adHoc",
            Self::Contract => "contract",
        }
    }
}

/// One published price.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Price {
    /// Which kind of price this is.
    pub price_type: PriceType,
    /// The amount, in the rate's currency.
    pub value: Decimal,
    /// The VAT percentage, when the component states one.
    pub tax_rate: Option<Decimal>,
    /// Whether the amount is gross. `None` where no tax regime applies.
    pub tax_included: Option<bool>,
    /// Free text the profile carries beside the number.
    ///
    /// Used only where the enumeration cannot state the whole truth — see
    /// [`RateNote`]. Written in English and published with an `en` language
    /// tag, because a `MultilingualString` labelled `de` carrying an English
    /// sentence is a worse document than an honestly labelled one; an operator
    /// that wants its own prose replaces the string.
    pub additional_information: Option<String>,
    /// The window this price applies in, when the tariff bounds one.
    pub period: Option<Period>,
    /// The delivered-energy band it applies in, when the tariff tiers on one.
    pub energy_applicability: Option<EnergyApplicability>,
    /// The elapsed-time band it applies in, when the tariff tiers on one.
    pub time_applicability: Option<TimeApplicability>,
}

/// A price's delivered-energy band, in the whole kWh the profile carries
/// `[DATEX-II-Profil]` (`EnergyPrice.energyBasedApplicability`).
///
/// The bounds are narrowed to integers rather than rounded to the nearest, so
/// the published band is always a subset of the real one and the document never
/// states a price where it does not apply. See the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EnergyApplicability {
    /// `fromKWh` — the price is valid from this much delivered.
    pub from_kwh: Option<u32>,
    /// `toKWh` — and up to this much.
    pub to_kwh: Option<u32>,
}

/// A price's elapsed-time band, in the whole minutes the profile carries
/// `[DATEX-II-Profil]` (`EnergyPrice.timeBasedApplicability`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimeApplicability {
    /// `fromMinute` — the price is valid from this minute of the session.
    pub from_minute: Option<u32>,
    /// `toMinute` — and up to this one.
    pub to_minute: Option<u32>,
}

impl EnergyApplicability {
    /// Whether it bounds anything at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.from_kwh.is_none() && self.to_kwh.is_none()
    }
}

impl TimeApplicability {
    /// Whether it bounds anything at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.from_minute.is_none() && self.to_minute.is_none()
    }
}

/// A validity window, as the profile's `overallPeriod` states one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Period {
    /// The first instant.
    pub from: Option<time::OffsetDateTime>,
    /// The last instant.
    pub until: Option<time::OffsetDateTime>,
    /// A recurring time of day, when the tariff restricts one.
    pub daily: Option<(time::Time, time::Time)>,
}

impl Period {
    /// Whether it bounds anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.from.is_none() && self.until.is_none() && self.daily.is_none()
    }
}

/// A published rate: a policy, a currency, and the prices under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rate {
    /// The identity the status publication addresses this rate by.
    pub id: String,
    /// Ad hoc or contract.
    pub policy: RatePolicy,
    /// The currency the prices are quoted in.
    pub currency: Currency,
    /// A name for the rate, when the tariff has one worth showing.
    pub name: Option<String>,
    /// The prices.
    pub prices: Vec<Price>,
    /// A session under this rate costs at least this much.
    pub minimum_delivery_fee: Option<Decimal>,
    /// A session under this rate costs at most this much.
    pub maximum_delivery_fee: Option<Decimal>,
    /// Whether charging and parking are one combined fee.
    ///
    /// The profile's only acknowledgement that parking might be priced
    /// `[DATEX-II-Profil Tab. 8]`. Set when the tariff prices occupancy,
    /// because the alternative — leaving it unset — tells a consumer the
    /// parking is free.
    pub combination_with_parking_fee: Option<bool>,
}

/// Something true about the tariff that the published rate does not say.
///
/// Not warnings about bad input: every one of these is a correct tariff that
/// `[DATEX-II-Profil]` has no vocabulary for. They are surfaced so an operator
/// can decide what to do about a feed that under-describes its own prices —
/// and so nobody has to rediscover the gaps by reading the enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RateNote {
    /// An occupancy fee was published as `other`, because there is no literal.
    ///
    /// `[AFIR Art. 5(4)]` permits it by name; `[DATEX-II-Profil Tab. A.116]`
    /// cannot express it. A consumer reading `other` learns that there is a
    /// further charge and not what it is for.
    OccupancyFeeHasNoPriceType {
        /// The fee, per minute, as published.
        per_minute: Decimal,
    },
    /// An hourly price does not divide exactly into a per-minute one.
    ///
    /// The published number is the nearest the currency's own scale allows, so
    /// it is not the price the session is rated at. The exact hourly price goes
    /// into `additionalInformation` beside it, because a consumer that can read
    /// only the number should at least be able to find the truth next to it.
    HourlyPriceIsNotExactPerMinute {
        /// What the tariff charges per hour.
        hourly: Decimal,
        /// What the feed states per minute.
        published: Decimal,
    },
    /// A tier boundary had to be narrowed to reach a whole unit.
    ///
    /// `fromKWh`, `toKWh`, `fromMinute` and `toMinute` are non-negative
    /// integers and a tariff's thresholds are not. A lower bound rounds up and
    /// an upper bound rounds down, so the published band is a subset of the real
    /// one — every statement in the document stays true, and the band it states
    /// is narrower than the one that was charged. The exact figure is here so an
    /// operator can see which tariff produced a feed it cannot state exactly.
    BoundNarrowedToWholeUnits {
        /// Which field — `fromKWh`, `toKWh`, `fromMinute` or `toMinute`.
        field: &'static str,
        /// What the tariff restricts on, in the unit the field is stated in —
        /// kWh, or minutes for a duration the tariff itself holds in seconds.
        exact: Decimal,
        /// What the feed states.
        published: u32,
    },
    /// A tier's whole band falls inside one unit, so the profile cannot state
    /// it at all.
    ///
    /// Narrowing both ends of a band narrower than a kilowatt-hour — or than a
    /// minute — leaves an empty one, and an empty band published as a bound is
    /// a price that applies nowhere. It is omitted instead, which makes the
    /// price read as unconditional; that is the honest failure and it is
    /// reported here.
    BandTooNarrowToPublish {
        /// Which applicability — `energyBasedApplicability` or
        /// `timeBasedApplicability`.
        applicability: &'static str,
        /// The lower bound, in the tariff's own unit.
        from: Decimal,
        /// The upper bound.
        to: Decimal,
    },
    /// A price applies only under conditions the published rate omits.
    ///
    /// A tariff tier restricted by *power* or by *weekday* has no equivalent in
    /// the profile — its `EnergyBasedApplicability` and `TimeBasedApplicability`
    /// cover delivered energy and elapsed time and nothing else — and a price
    /// published without its condition reads as unconditional.
    RestrictionNotPublished {
        /// Which condition was dropped.
        restriction: &'static str,
    },
}

/// Sixty minutes in an hour — the whole of the per-hour to per-minute problem.
const MINUTES_PER_HOUR: Decimal = Decimal::from_parts(60, 0, 0, false, 0);

/// Sixty seconds in a minute. `emob-tariff` restricts on seconds and the
/// profile's `fromMinute`/`toMinute` are whole minutes.
const SECONDS_PER_MINUTE: Decimal = Decimal::from_parts(60, 0, 0, false, 0);

/// The rate a tariff publishes, and everything the profile could not carry.
///
/// `id` becomes the rate's `idG`, which the **status** publication uses to send
/// a price update without republishing the table — so it has to be stable
/// across exports for the same tariff. Deriving it from
/// [`emob_tariff::TariffFingerprint`] gives an id that changes exactly when the
/// prices do, which is the behaviour a consumer's cache wants.
#[must_use]
pub fn publish(tariff: &Tariff, id: impl Into<String>) -> (Rate, Vec<RateNote>) {
    let mut notes = Vec::new();
    let mut prices = Vec::new();
    let mut prices_parking = false;

    let tax_included = match tariff.tax_included {
        TaxIncluded::Yes => Some(true),
        TaxIncluded::No => Some(false),
        TaxIncluded::NotApplicable => None,
    };

    for element in &tariff.elements {
        let period = period_of(tariff, element, &mut notes);
        let energy = energy_applicability_of(&element.restrictions, &mut notes);
        let time = time_applicability_of(&element.restrictions, &mut notes);
        for component in &element.components {
            if component.dimension == Dimension::ParkingTime {
                prices_parking = true;
            }
            prices.push(price_of(
                component,
                tariff.currency,
                tax_included,
                period.clone(),
                energy,
                time,
                &mut notes,
            ));
        }
    }

    let rate = Rate {
        id: id.into(),
        policy: match tariff.kind {
            TariffKind::AdHoc => RatePolicy::AdHoc,
            TariffKind::Contract => RatePolicy::Contract,
        },
        currency: tariff.currency,
        name: None,
        prices,
        minimum_delivery_fee: tariff.min_price,
        maximum_delivery_fee: tariff.max_price,
        combination_with_parking_fee: prices_parking.then_some(false),
    };
    (rate, notes)
}

/// One component, in the profile's vocabulary.
fn price_of(
    component: &PriceComponent,
    currency: Currency,
    tax_included: Option<bool>,
    period: Option<Period>,
    energy_applicability: Option<EnergyApplicability>,
    time_applicability: Option<TimeApplicability>,
    notes: &mut Vec<RateNote>,
) -> Price {
    let (price_type, value, additional_information) = match component.dimension {
        Dimension::Energy => (PriceType::PricePerKwh, component.price, None),
        Dimension::Flat => (PriceType::FlatRate, component.price, None),
        Dimension::Time => {
            let per_minute = component.price / MINUTES_PER_HOUR;
            let information = if per_minute * MINUTES_PER_HOUR == component.price {
                None
            } else {
                notes.push(RateNote::HourlyPriceIsNotExactPerMinute {
                    hourly: component.price,
                    published: per_minute,
                });
                Some(format!(
                    "charging time is priced at {} per hour",
                    component.price
                ))
            };
            (PriceType::PricePerMinute, per_minute, information)
        }
        // The one surcharge `[AFIR Art. 5(4)]` names, and the one the profile
        // has no literal for. `other` plus a sentence is the whole truth this
        // vocabulary can carry.
        Dimension::ParkingTime => {
            let per_minute = component.price / MINUTES_PER_HOUR;
            notes.push(RateNote::OccupancyFeeHasNoPriceType { per_minute });
            (
                PriceType::Other,
                per_minute,
                Some(format!(
                    "occupancy fee for time connected and not charging, \
                     {} {currency} per hour [AFIR Art. 5(4)]",
                    component.price
                )),
            )
        }
    };

    Price {
        price_type,
        value,
        tax_rate: component.vat,
        tax_included,
        additional_information,
        period: period.filter(|period| !period.is_empty()),
        energy_applicability,
        time_applicability,
    }
}

/// The delivered-energy band an element applies in, narrowed to whole kWh.
///
/// `fromKWh` rounds **up** and `toKWh` rounds **down**, so the published band is
/// a subset of the tariff's: a document that states a narrower band than the one
/// charged is imprecise, and one that states a wider band is wrong.
fn energy_applicability_of(
    restrictions: &emob_tariff::Restrictions,
    notes: &mut Vec<RateNote>,
) -> Option<EnergyApplicability> {
    let from = restrictions
        .min_kwh
        .map(|min| whole_unit(min, "fromKWh", Bound::Lower, notes));
    let to = restrictions
        .max_kwh
        .map(|max| whole_unit(max, "toKWh", Bound::Upper, notes));

    let band = EnergyApplicability {
        from_kwh: from,
        to_kwh: to,
    };
    if band.is_empty() {
        return None;
    }
    // Narrowing both ends of a band under a kilowatt-hour wide leaves an empty
    // one, and a price bounded to nowhere is worse than an unbounded price.
    if let (Some(from), Some(to)) = (from, to)
        && from >= to
    {
        notes.push(RateNote::BandTooNarrowToPublish {
            applicability: "energyBasedApplicability",
            from: restrictions.min_kwh.unwrap_or_default(),
            to: restrictions.max_kwh.unwrap_or_default(),
        });
        return None;
    }
    Some(band)
}

/// The elapsed-time band an element applies in, narrowed to whole minutes.
fn time_applicability_of(
    restrictions: &emob_tariff::Restrictions,
    notes: &mut Vec<RateNote>,
) -> Option<TimeApplicability> {
    let minutes = |seconds: u64| Decimal::from(seconds) / SECONDS_PER_MINUTE;
    let from = restrictions
        .min_duration_s
        .map(|min| whole_unit(minutes(min), "fromMinute", Bound::Lower, notes));
    let to = restrictions
        .max_duration_s
        .map(|max| whole_unit(minutes(max), "toMinute", Bound::Upper, notes));

    let band = TimeApplicability {
        from_minute: from,
        to_minute: to,
    };
    if band.is_empty() {
        return None;
    }
    if let (Some(from), Some(to)) = (from, to)
        && from >= to
    {
        notes.push(RateNote::BandTooNarrowToPublish {
            applicability: "timeBasedApplicability",
            from: Decimal::from(restrictions.min_duration_s.unwrap_or_default()),
            to: Decimal::from(restrictions.max_duration_s.unwrap_or_default()),
        });
        return None;
    }
    Some(band)
}

/// Which way a bound has to move to keep the published band inside the real one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bound {
    /// A lower bound rounds up.
    Lower,
    /// An upper bound rounds down.
    Upper,
}

/// One bound, narrowed to a whole unit, reporting the move.
fn whole_unit(exact: Decimal, field: &'static str, bound: Bound, notes: &mut Vec<RateNote>) -> u32 {
    use rust_decimal::prelude::ToPrimitive as _;

    let narrowed = match bound {
        Bound::Lower => exact.ceil(),
        Bound::Upper => exact.floor(),
    };
    let published = narrowed.to_u32().unwrap_or(u32::MAX);
    if narrowed.cmp(&exact) != Ordering::Equal {
        notes.push(RateNote::BoundNarrowedToWholeUnits {
            field,
            exact,
            published,
        });
    }
    published
}

/// The window an element applies in, reporting what could not be carried.
fn period_of(
    tariff: &Tariff,
    element: &emob_tariff::TariffElement,
    notes: &mut Vec<RateNote>,
) -> Option<Period> {
    let restrictions = &element.restrictions;

    // The energy and duration bounds are published, in whole units, by
    // `energy_applicability_of` and `time_applicability_of`. What is left here
    // is what the profile has no field for at all.
    for (present, name) in [
        (restrictions.min_power_kw.is_some(), "a minimum power"),
        (restrictions.max_power_kw.is_some(), "a maximum power"),
        (!restrictions.days_of_week.is_empty(), "certain weekdays"),
    ] {
        if present {
            notes.push(RateNote::RestrictionNotPublished { restriction: name });
        }
    }

    let daily = match (restrictions.start_time, restrictions.end_time) {
        (Some(start), Some(end)) => Some((start, end)),
        (Some(start), None) => Some((start, time::Time::MIDNIGHT)),
        (None, Some(end)) => Some((time::Time::MIDNIGHT, end)),
        (None, None) => None,
    };

    let period = Period {
        from: tariff.valid_from,
        until: tariff.valid_until,
        daily,
    };
    (!period.is_empty()).then_some(period)
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_tariff::{TariffElement, TariffKind};

    fn dec(s: &str) -> Decimal {
        Decimal::from_str_exact(s).unwrap()
    }

    fn tariff_of(components: Vec<PriceComponent>) -> Tariff {
        Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            tax_included: TaxIncluded::Yes,
            elements: vec![TariffElement::unrestricted(components)],
            min_price: None,
            max_price: None,
            valid_from: None,
            valid_until: None,
        }
    }

    #[test]
    fn the_energy_price_is_the_tariffs_own_decimal() {
        // Not a formatted string, not a float, not a second computation of the
        // same number: the value the rating engine charges with.
        let (rate, notes) = publish(
            &tariff_of(vec![PriceComponent::new(Dimension::Energy, dec("0.4900"))]),
            "r",
        );
        assert!(notes.is_empty());
        assert_eq!(rate.prices[0].price_type, PriceType::PricePerKwh);
        assert_eq!(rate.prices[0].value.to_string(), "0.4900");
        assert_eq!(rate.policy, RatePolicy::AdHoc);
        assert_eq!(rate.prices[0].tax_included, Some(true));
    }

    #[test]
    fn an_hourly_price_becomes_a_per_minute_one_because_the_profile_has_no_other_type() {
        // The factor-sixty trap: `pricePerMinute` is the only time literal
        // `[DATEX-II-Profil Tab. A.116]` has, and a tariff is written per hour.
        // Publishing the hourly number under it overstates the price sixtyfold.
        let (rate, notes) = publish(
            &tariff_of(vec![PriceComponent::new(Dimension::Time, dec("0.60"))]),
            "r",
        );
        assert!(notes.is_empty(), "0.60/60 is exact");
        assert_eq!(rate.prices[0].price_type, PriceType::PricePerMinute);
        assert_eq!(rate.prices[0].value, dec("0.01"));
    }

    #[test]
    fn an_hourly_price_that_does_not_divide_says_so_next_to_the_number() {
        let (rate, notes) = publish(
            &tariff_of(vec![PriceComponent::new(Dimension::Time, dec("0.25"))]),
            "r",
        );
        assert!(matches!(
            notes.as_slice(),
            [RateNote::HourlyPriceIsNotExactPerMinute { .. }]
        ));
        assert!(
            rate.prices[0]
                .additional_information
                .as_deref()
                .is_some_and(|text| text.contains("0.25")),
            "the exact hourly price has to be findable beside the rounded one"
        );
    }

    #[test]
    fn the_occupancy_fee_afir_names_has_no_price_type_to_be_published_under() {
        // `[AFIR Art. 5(4)]` permits an occupancy fee by name. The profile the
        // same Regulation requires under Art. 20 has no literal for it. This
        // is the finding, and the test is what keeps it from being forgotten.
        let (rate, notes) = publish(
            &tariff_of(vec![
                PriceComponent::new(Dimension::Energy, dec("0.49")),
                PriceComponent::new(Dimension::ParkingTime, dec("6.00")),
            ]),
            "r",
        );

        assert_eq!(rate.prices[1].price_type, PriceType::Other);
        assert_eq!(rate.prices[1].value, dec("0.1"));
        assert!(matches!(
            notes.as_slice(),
            [RateNote::OccupancyFeeHasNoPriceType { .. }]
        ));

        // …and the one boolean the profile does have is set, because leaving it
        // unset tells a consumer the parking is free.
        assert_eq!(rate.combination_with_parking_fee, Some(false));
    }

    #[test]
    fn a_condition_the_profile_cannot_carry_is_reported_rather_than_dropped() {
        // A price published without its "only above 30 kW" condition reads as
        // unconditional, which is a price transparency problem and not a
        // formatting one `[AFIR Art. 5(2)]`.
        let mut element =
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.39"))]);
        element.restrictions.min_power_kw = Some(dec("30"));
        let mut tariff = tariff_of(vec![]);
        tariff.elements = vec![element];

        let (_, notes) = publish(&tariff, "r");
        assert!(matches!(
            notes.as_slice(),
            [RateNote::RestrictionNotPublished {
                restriction: "a minimum power"
            }]
        ));
    }

    #[test]
    fn a_time_of_day_restriction_does_survive() {
        let mut element =
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.30"))]);
        element.restrictions.start_time = Some(time::macros::time!(21:00));
        element.restrictions.end_time = Some(time::macros::time!(6:00));
        let mut tariff = tariff_of(vec![]);
        tariff.elements = vec![element];

        let (rate, notes) = publish(&tariff, "r");
        assert!(notes.is_empty());
        assert_eq!(
            rate.prices[0].period.as_ref().unwrap().daily,
            Some((time::macros::time!(21:00), time::macros::time!(6:00)))
        );
    }
}
