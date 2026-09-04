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
//! - **A delivery fee cannot say whether tax is in it.** Every `EnergyPrice`
//!   carries `taxIncluded` and `taxRate`; `EnergyRate`'s `minimumDeliveryFee`
//!   and `maximumDeliveryFee` are a bare `AmountOfMoney` with neither. The one
//!   figure on the rate that is a session **total** rather than a unit price is
//!   therefore the one a consumer cannot qualify — and reading a net minimum as
//!   gross is out by the whole VAT rate, on the number a driver most wants to
//!   compare.
//!
//! None of these is a reason to refuse to publish. All of them are reasons to
//! say so, which is what [`RateNote`] is for.
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
    /// The zone every [`Period::daily`] window on this rate is read in — the
    /// IANA name of [`emob_tariff::Tariff::time_zone`].
    ///
    /// Carried because the document cannot carry it: the profile's own field is
    /// an ISO 8601 offset on the site, which cannot express summer time. Keeping
    /// the real name here is what lets [`crate::feed::Feed::check`] refuse a
    /// rate offered at a site on a different clock — a `22:00` night rate
    /// published under a site an hour away is a price the driver at that site is
    /// never charged.
    pub time_zone: String,
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
        /// The lower bound, in the unit the **field** is stated in — kWh, or
        /// minutes for a duration the tariff itself holds in seconds. The same
        /// unit [`Self::BoundNarrowedToWholeUnits`] reports, because two scales
        /// for one restriction in one report is a report nobody can act on.
        from: Decimal,
        /// The upper bound, in the same unit.
        to: Decimal,
    },
    /// An element that prices a **reservation** was left out of the feed.
    ///
    /// `[DATEX-II-Profil]`'s `EnergyRate` states what recharging costs. It has
    /// no reservation in it — the profile is about the delivery of energy, and a
    /// reservation runs before any is delivered — so publishing the element's
    /// price would put a reservation's rate per hour into the national access
    /// point as the price of charging. It is omitted, and named here, because a
    /// price the public reads as one thing and the invoice charges for another
    /// is the drift this whole crate exists to prevent (D243).
    ReservationPriceNotPublished {
        /// Which outcome the element priced, in `[OCPI 2.3.0]`'s own token.
        outcome: &'static str,
        /// How many price components went with it.
        components: usize,
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
    /// A minimum or maximum delivery fee is published with no tax basis.
    ///
    /// `EnergyPrice` carries `taxIncluded` and `taxRate`; `EnergyRate`'s
    /// `minimumDeliveryFee` and `maximumDeliveryFee` are a bare `AmountOfMoney`
    /// with neither. So the one figure on the rate that is a **total** rather
    /// than a unit price is the one the profile cannot say whether tax is inside
    /// — and a consumer reading a net minimum as gross is out by the whole VAT
    /// rate on the number a driver most wants to compare.
    ///
    /// Raised only where the tariff actually states a basis. A party outside a
    /// tax regime has nothing to say and nothing is lost.
    DeliveryFeeHasNoTaxBasis {
        /// Which bound — `minimumDeliveryFee` or `maximumDeliveryFee`.
        field: &'static str,
        /// The figure as published.
        amount: Decimal,
        /// Whether that figure includes tax, which the document cannot state.
        tax_included: bool,
    },
    /// A price applies in a daily window, and the profile can only say which
    /// zone that window is read in as a **fixed offset**.
    ///
    /// The window itself publishes fine: `overallPeriod` carries a
    /// `TimePeriodOfDay`, and `22:00` is `22:00`. What it does not carry is the
    /// zone — that lives one object away, on `FacilityLocation.timeZone`, and
    /// the profile types it as a string that "identifies a time zone by
    /// specifying the difference to UTC in hours and minutes, as defined in
    /// ISO 8601" `[DATEX-II-Profil]`. Its own reference instance publishes
    /// `"+01:00"` for a German site.
    ///
    /// An offset is not a zone. `+01:00` is wrong for that site from the last
    /// Sunday in March to the last Sunday in October, so a consumer reading a
    /// `22:00` night rate against the published offset prices an hour of every
    /// summer evening at the wrong rate — the same failure the rating engine
    /// carries a [`emob_core::TimeZone`] to avoid, met from the publishing
    /// side. The table publishes the offset in force when it is issued, which
    /// is the only reading that is true of the document, and this says that the
    /// document has no way to state the rest.
    DailyWindowHasOnlyAnOffset {
        /// The zone the tariff's wall clock is actually read in.
        zone: String,
        /// The window, as published.
        from: time::Time,
        /// …and its end.
        to: time::Time,
    },
}

impl core::fmt::Display for RateNote {
    /// One line per note, for an operator queue — the same shape
    /// [`emob_tariff::RatingNote`] and `emob_core::Note` carry, because a gap
    /// in a published price and a gap in a rated one land in the same inbox.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OccupancyFeeHasNoPriceType { per_minute } => write!(
                f,
                "the occupancy fee of {per_minute} per minute is published as `other`: [AFIR Art. 5(4)] permits it by name and [DATEX-II-Profil Tab. A.116] has no price type for parking"
            ),
            Self::HourlyPriceIsNotExactPerMinute { hourly, published } => write!(
                f,
                "{hourly} per hour has no exact price per minute; the feed states {published} and the exact hourly figure is in `additionalInformation` beside it"
            ),
            Self::BoundNarrowedToWholeUnits {
                field,
                exact,
                published,
            } => write!(
                f,
                "{field} carries whole units only: the tariff's {exact} is published as {published}, so the band stated is a subset of the band charged"
            ),
            Self::BandTooNarrowToPublish {
                applicability,
                from,
                to,
            } => write!(
                f,
                "the band {from}–{to} is narrower than one whole unit, so {applicability} is omitted and the price reads as unconditional"
            ),
            Self::ReservationPriceNotPublished {
                outcome,
                components,
            } => write!(
                f,
                "{components} price component(s) price a {outcome} and are not published: [DATEX-II-Profil]'s EnergyRate states what recharging costs, and a reservation runs before any energy is delivered"
            ),
            Self::RestrictionNotPublished { restriction } => write!(
                f,
                "this price applies only under a {restriction} condition, which [DATEX-II-Profil] cannot state: the published price reads as unconditional"
            ),
            Self::DeliveryFeeHasNoTaxBasis {
                field,
                amount,
                tax_included,
            } => write!(
                f,
                "{field} is published as {amount} {}: [DATEX-II-Profil] gives `EnergyPrice` a `taxIncluded` flag and gives the delivery fee none, so a consumer cannot tell which, and reading it the other way is out by the whole VAT rate",
                if *tax_included {
                    "including tax"
                } else {
                    "excluding tax"
                }
            ),
            Self::DailyWindowHasOnlyAnOffset { zone, from, to } => write!(
                f,
                "this price applies from {from} to {to} on the wall clock of {zone}, and [DATEX-II-Profil] carries that zone only as an ISO 8601 offset on `FacilityLocation.timeZone` — which cannot express summer time, so the table states the offset in force when it is published and a consumer reading the window six months later reads it an hour out"
            ),
        }
    }
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
        // A reservation is not recharging, and the profile states what
        // recharging costs. See `RateNote::ReservationPriceNotPublished`.
        if let Some(outcome) = element.restrictions.reservation {
            notes.push(RateNote::ReservationPriceNotPublished {
                outcome: outcome.as_str(),
                components: element.components.len(),
            });
            continue;
        }
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

    // The delivery fees carry no tax flag of their own — the profile gives one
    // to every `EnergyPrice` and none to `EnergyRate`'s two bounds — so the one
    // figure on the rate that is a session *total* is the one it cannot qualify.
    if let Some(included) = tax_included {
        for (field, bound) in [
            (
                "minimumDeliveryFee",
                tariff
                    .min_price
                    .and_then(|p| p.in_basis(tariff.tax_included)),
            ),
            (
                "maximumDeliveryFee",
                tariff
                    .max_price
                    .and_then(|p| p.in_basis(tariff.tax_included)),
            ),
        ] {
            if let Some(amount) = bound {
                notes.push(RateNote::DeliveryFeeHasNoTaxBasis {
                    field,
                    amount,
                    tax_included: included,
                });
            }
        }
    }

    let rate = Rate {
        id: id.into(),
        policy: match tariff.kind {
            TariffKind::AdHoc => RatePolicy::AdHoc,
            TariffKind::Contract => RatePolicy::Contract,
        },
        time_zone: tariff.time_zone.to_string(),
        currency: tariff.currency,
        name: None,
        prices,
        // The feed has one field per bound and `[OCPI 2.3.0
        // §mod_tariffs_pricelimit_class]` has two figures, so what the public
        // reads is the bound in the tariff's own basis — the figure a driver
        // compares the price they were shown against. The other limb still
        // binds; it is the rating that enforces it.
        minimum_delivery_fee: tariff
            .min_price
            .and_then(|p| p.in_basis(tariff.tax_included)),
        maximum_delivery_fee: tariff
            .max_price
            .and_then(|p| p.in_basis(tariff.tax_included)),
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
            // In **minutes**, the unit the field is stated in and the unit
            // `BoundNarrowedToWholeUnits` reports beside it. The tariff holds a
            // duration in seconds, and a note that said "the band 600–630 is
            // narrower than one whole unit" would put two scales on one
            // restriction in one report.
            applicability: "timeBasedApplicability",
            from: minutes(restrictions.min_duration_s.unwrap_or_default()),
            to: minutes(restrictions.max_duration_s.unwrap_or_default()),
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

    if let Some((from, to)) = daily {
        notes.push(RateNote::DailyWindowHasOnlyAnOffset {
            zone: tariff.time_zone.to_string(),
            from,
            to,
        });
    }

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
    use emob_tariff::PriceLimit;
    use emob_tariff::{TariffElement, TariffKind};

    fn dec(s: &str) -> Decimal {
        Decimal::from_str_exact(s).unwrap()
    }

    fn tariff_of(components: Vec<PriceComponent>) -> Tariff {
        Tariff {
            id: "t".parse().unwrap(),
            currency: Currency::EUR,
            kind: TariffKind::AdHoc,
            time_zone: emob_core::TimeZone::new("Europe/Berlin").unwrap(),
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
    fn a_band_narrower_than_a_whole_unit_reports_it_in_the_unit_the_field_uses() {
        // `fromMinute`/`toMinute` are whole minutes and the tariff holds a
        // duration in seconds. A note saying "the band 600–630 is narrower than
        // one whole unit" beside a `BoundNarrowedToWholeUnits` reported in
        // minutes puts two scales on one restriction in one report.
        let mut element =
            TariffElement::unrestricted(vec![PriceComponent::new(Dimension::Energy, dec("0.30"))]);
        element.restrictions.min_duration_s = Some(600); // 10 min
        element.restrictions.max_duration_s = Some(630); // 10.5 min
        let mut tariff = tariff_of(vec![]);
        tariff.elements = vec![element];

        let (_, notes) = publish(&tariff, "r");
        let narrow = notes
            .iter()
            .find_map(|note| match note {
                RateNote::BandTooNarrowToPublish { from, to, .. } => Some((*from, *to)),
                _ => None,
            })
            .expect("the band narrows to nothing and has to say so");
        assert_eq!(narrow, (dec("10"), dec("10.5")), "minutes, not seconds");
        assert!(
            notes
                .iter()
                .any(|note| note.to_string().contains("narrower than one whole unit"))
        );
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
        assert_eq!(
            rate.prices[0].period.as_ref().unwrap().daily,
            Some((time::macros::time!(21:00), time::macros::time!(6:00)))
        );
        // The window survives; the zone it is read in does not, because the
        // profile carries a fixed offset and a German site is not at one.
        assert_eq!(
            notes,
            vec![RateNote::DailyWindowHasOnlyAnOffset {
                zone: "Europe/Berlin".to_owned(),
                from: time::macros::time!(21:00),
                to: time::macros::time!(6:00),
            }]
        );
        assert!(notes[0].to_string().contains("Europe/Berlin"));
    }

    #[test]
    fn a_delivery_fee_is_published_with_a_tax_basis_the_profile_cannot_state() {
        // Every `EnergyPrice` carries `taxIncluded`; `EnergyRate`'s two bounds
        // carry nothing. So the one figure on the rate that is a session total
        // is the one a consumer cannot qualify, and reading a net minimum as
        // gross is out by the whole VAT rate.
        let mut tariff = tariff_of(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        tariff.tax_included = TaxIncluded::No;
        tariff.min_price = Some(PriceLimit::net(dec("5.00")));

        let (rate, notes) = publish(&tariff, "r");
        assert_eq!(rate.minimum_delivery_fee, Some(dec("5.00")));
        assert!(
            notes.iter().any(|note| matches!(
                note,
                RateNote::DeliveryFeeHasNoTaxBasis { field, tax_included: false, .. }
                    if *field == "minimumDeliveryFee"
            )),
            "{notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|note| note.to_string().contains("excluding tax")),
            "{notes:?}"
        );

        // A tariff that states no bound has nothing to qualify, and one outside
        // a tax regime has nothing to say.
        let (_, quiet) = publish(&tariff_of(vec![]), "r");
        assert!(quiet.is_empty());

        let mut outside = tariff_of(vec![]);
        outside.tax_included = TaxIncluded::NotApplicable;
        outside.max_price = Some(PriceLimit::net(dec("40.00")));
        let (_, none) = publish(&outside, "r");
        assert!(none.is_empty(), "{none:?}");
    }
}
