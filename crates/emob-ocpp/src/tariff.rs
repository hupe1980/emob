//! The tariff, carried to the screen the driver actually reads.
//!
//! # The audience nobody serves from the tariff engine
//!
//! `[AFIR Art. 5(4)]` requires the ad-hoc price to be "known to end users
//! **before they initiate** a recharging session". The place a driver learns it
//! is the charge point's own display, and until OCPP 2.1 there was no
//! structured way to put it there: 2.0.1 could send a `DisplayMessage` string
//! and a running `CostUpdated` number, both computed somewhere else. So the
//! price on the screen came from a field somebody typed, and the price on the
//! invoice came from the tariff engine — which is the drift `emob-tariff`
//! exists to make unrepresentable, surviving in the one place the Regulation
//! actually names.
//!
//! OCPP 2.1's *Tariff and Cost* block closes it. `SetDefaultTariff` installs a
//! structured tariff per EVSE, `ChangeTransactionTariff` swaps one mid-session,
//! and the station is required to show the tariff's own `description` to the
//! driver. This module is the crossing onto that object, and it is the third
//! audience of one inventory: the same [`emob_tariff::Tariff`] that rates the
//! CDR, that `emob-roam` publishes to a roaming partner, and that `emob-poi`
//! publishes to the national access point.
//!
//! # The two models are the same model
//!
//! OCPI orders *elements* and picks, per dimension, the first whose
//! restrictions match `[OCPI 2.3.0 §Tariff]`. OCPP 2.1 orders *prices* within
//! each dimension and picks the first whose conditions match
//! `[OCPP 2.1 Part 2, TariffEnergyPrice]`. Projecting this workspace's element
//! list onto one price list per dimension — in order, keeping each element's
//! restrictions as that price's conditions — is exactly the projection
//! [`emob_tariff::matching_component`] already performs at rating time. The
//! station therefore selects the component the invoice will be built from, by
//! construction rather than by agreement.
//!
//! # …and where they are not, the crossing says so
//!
//! | Fact | OCPP 2.1 | What happens |
//! |---|---|---|
//! | a time price | `priceMinute`, per **minute** | an hourly rate with no exact per-minute spelling is **refused**: `[AFIR Art. 5(4)]` asks for the per-minute figure and a rounded one is not the price charged |
//! | a price | excluding tax, with `taxRates` beside it | a gross tariff's net is computed at the wire's own precision, and the residual the station will re-derive is noted |
//! | tax | one `taxRates` list per **dimension** | a dimension charged at two VAT rates is **refused**: one rate carried for both taxes the second tier at the first tier's rate, on the wire |
//! | a flat fee's conditions | `TariffConditionsFixed` — wall clock only | a session fee restricted on energy, power or duration is **refused**: dropping the condition widens it at the station |
//! | `step_size` | no field at all | noted: the station bills the quantity it measured |
//! | `valid_until` | no field at all | noted: a version that ends has to be cleared by the CSMS |
//! | an unevaluable restriction | — | **refused**, the same rule as the OCPI crossing: publishing it stripped widens the element at the receiver |
//!
//! A refusal rather than a note wherever the loss would be **in the driver's
//! disfavour and invisible**, which is the rule `emob-roam` states and the same
//! rule stated here.

use emob_core::Crossing;
use emob_tariff::{Dimension, Restrictions, Tariff, TariffElement, TaxIncluded, price_per_minute};
use ocpp_kit::types::Decimal as Wire;
use ocpp_kit::v2_1::{
    DayOfWeek, MessageContent, MessageFormat, Price, TariffConditions, TariffConditionsFixed,
    TariffEnergy, TariffEnergyPrice, TariffFixed, TariffFixedPrice, TariffTime, TariffTimePrice,
    TaxRate,
};
use rust_decimal::Decimal;

use crate::error::SeamError;

/// A thousand, exactly — kWh to Wh, kW to W.
const THOUSAND: Decimal = Decimal::from_parts(1000, 0, 0, false, 0);
/// How many lines `[OCPP 2.1 Part 2, TariffType]`'s `description` list holds.
const MAX_DESCRIPTION_LINES: usize = 10;
/// Percent.
const HUNDRED: Decimal = Decimal::from_parts(100, 0, 0, false, 0);

/// Carry a tariff onto OCPP 2.1's `TariffType`, for `SetDefaultTariff` or
/// `ChangeTransactionTariff`.
///
/// `at` is the instant the description is written for — a tariff with
/// time-of-day elements describes differently at different hours, and a
/// station being provisioned at 21:58 for a price that changes at 22:00 needs
/// the caller to say which. It is an argument rather than a clock because a
/// provisioning run replayed two years later has to produce the same bytes.
///
/// # Errors
///
/// [`SeamError::TariffNotCarriedByOcpp`] for every loss that would be in the
/// driver's disfavour and invisible in the document — see the module table —
/// and [`SeamError::UnrepresentableDecimal`] for a figure outside the wire's
/// range.
pub fn to_ocpp(
    tariff: &Tariff,
    at: time::OffsetDateTime,
) -> Result<Crossing<ocpp_kit::v2_1::Tariff>, SeamError> {
    let mut crossing = Crossing::lossless(());

    for (index, element) in tariff.elements.iter().enumerate() {
        if let Some(restriction) = element.restrictions.unevaluable.first() {
            return Err(SeamError::TariffNotCarriedByOcpp {
                pointer: format!("/elements/{index}/restrictions"),
                reason: format!(
                    "this element restricts on {restriction}, which this build cannot evaluate. \
                     Publishing it stripped does not narrow the element, it widens it: the station \
                     would then apply the price wherever the rest of the conditions hold"
                ),
            });
        }
    }

    let energy = prices(tariff, Dimension::Energy, &mut crossing)?;
    let charging_time = prices(tariff, Dimension::Time, &mut crossing)?;
    let idle_time = prices(tariff, Dimension::ParkingTime, &mut crossing)?;
    let fixed = prices(tariff, Dimension::Flat, &mut crossing)?;

    // The half of `[AFIR Art. 5(4)]` a wire can actually deliver: OCPP 2.1
    // requires a station to show `description` to the driver, and this is the
    // same disclosure `emob_tariff::describe` derives from the tariff that
    // rates. Every tier, with its conditions, because "all its price
    // components" means all of them — and in the tariff's own basis, so the
    // number a driver reads is the number they pay whatever the wire quotes
    // prices in.
    let disclosure = emob_tariff::describe(tariff, at).full_disclosure();
    let tiers = disclosure.len();
    let description: Vec<MessageContent> = disclosure
        .into_iter()
        .take(MAX_DESCRIPTION_LINES)
        .map(|line| MessageContent::new(MessageFormat::UTF8, line))
        .collect();
    // …and OCPP's list holds ten `[OCPP 2.1 Part 2, TariffType]`, which a
    // tariff with more tiers than that overflows.
    //
    // A note rather than a refusal, and the line the refusals are drawn on is
    // what decides it: the four above are cases where the **station would
    // charge differently**, and no note repairs a number the driver is
    // entitled to read at face value. This one changes nothing that is
    // charged — the prices and their conditions all cross — and refusing would
    // leave the station displaying no price at all, which is a worse breach of
    // the same article than displaying ten tiers of twelve. So the operator is
    // told, by a note that names the article, and the decision is theirs.
    if tiers > MAX_DESCRIPTION_LINES {
        let dropped = tiers - MAX_DESCRIPTION_LINES;
        crossing.note(
            "/description",
            format!(
                "this tariff has {tiers} tiers and OCPP 2.1's description holds \
                 {MAX_DESCRIPTION_LINES}: the {dropped} the station cannot show are dropped. \
                 [AFIR Art. 5(4)] asks for all of a tariff's price components, so a tariff this \
                 deep cannot state its own disclosure at the point"
            ),
        );
    }

    let mut built = ocpp_kit::v2_1::Tariff::new(tariff.id.as_str(), tariff.currency.as_str());
    built.description = (!description.is_empty()).then_some(description);
    built.energy = energy.map(|d| set_tax(TariffEnergy::new(d.prices), d.tax));
    built.charging_time = charging_time.map(|d| set_tax(TariffTime::new(d.prices), d.tax));
    built.idle_time = idle_time.map(|d| set_tax(TariffTime::new(d.prices), d.tax));
    built.fixed_fee = fixed.map(|d| set_tax(TariffFixed::new(d.prices), d.tax));
    built.valid_from = tariff.valid_from.map(instant).transpose()?;
    built.min_cost = tariff
        .min_price
        .map(|bound| cost(bound, tariff, "/minCost", &mut crossing))
        .transpose()?;
    built.max_cost = tariff
        .max_price
        .map(|bound| cost(bound, tariff, "/maxCost", &mut crossing))
        .transpose()?;

    // A tariff version's *end* has no field in OCPP 2.1: `validFrom` exists and
    // nothing matches `validUntil`. A station handed a version that expires
    // keeps charging under it, so the expiry is the CSMS's to enforce with
    // `ClearTariffs` or the next `SetDefaultTariff` — and an operator who has
    // not been told that will find out from an invoice.
    if let Some(until) = tariff.valid_until {
        crossing.note(
            "/validFrom",
            format!(
                "this version stops being in force at {until} and OCPP 2.1 has no field for that: \
                 a station keeps charging under the tariff it holds. The CSMS has to replace or \
                 clear it [OCPP 2.1 Part 2, SetDefaultTariff]"
            ),
        );
    }

    // The last question, asked of the finished document rather than of the
    // tariff: does it satisfy the schema the station validates against? Every
    // bound above is enumerated here from the specification, and an enumeration
    // is a list somebody maintains — a tariff id longer than sixty characters,
    // a condition string this build formats wrongly, a bound a future
    // `ocpp-kit` tightens. The kit already owns the schema, so the answer is
    // its to give, and a document refused here is one refused before a station
    // ever sees it instead of after.
    if let Err(violations) = ocpp_kit::validate::Validate::validate(&built) {
        let first = violations
            .as_slice()
            .first()
            .map_or_else(String::new, ToString::to_string);
        return Err(SeamError::TariffNotCarriedByOcpp {
            pointer: String::new(),
            reason: format!(
                "the document does not satisfy OCPP 2.1's own schema ({} violation(s), the first \
                 is {first})",
                violations.len()
            ),
        });
    }

    Ok(crossing.map(|()| built))
}

/// The running cost a station displays, from the rating that will invoice it.
///
/// `CostUpdated.totalCost` is the cost "including taxes"
/// `[OCPP 2.1 Part 2, CostUpdatedRequest]`, so it is
/// [`emob_tariff::Rated::gross`] — the same figure the CDR carries, rounded the
/// way an invoice rounds it: per VAT category, then summed. A station showing a
/// running total computed any other way is showing a number the invoice will
/// not match, which is the whole failure this seam exists to prevent.
///
/// # Errors
///
/// [`SeamError::UnrepresentableDecimal`] for a total outside the wire's range.
pub fn cost_updated(rated: &emob_tariff::Rated) -> Result<Wire, SeamError> {
    let gross = rated.gross().amount();
    let (wire, carried) = to_wire(gross).ok_or_else(|| SeamError::UnrepresentableDecimal {
        pointer: "/totalCost".to_owned(),
        value: gross.to_string(),
    })?;
    debug_assert_eq!(
        carried, gross,
        "a rounded money total already fits the wire"
    );
    Ok(wire)
}

/// Attach a dimension's tax rate, when it has one.
fn set_tax<T>(prices: T, tax: Option<TaxRate>) -> T
where
    T: TaxRates,
{
    match tax {
        Some(rate) => prices.with_rates(vec![rate]),
        None => prices,
    }
}

/// The three OCPP tariff dimensions that carry a `taxRates` list.
trait TaxRates {
    /// The same value with its tax rates set.
    fn with_rates(self, rates: Vec<TaxRate>) -> Self;
}

macro_rules! impl_tax_rates {
    ($($ty:ty),+) => {$(
        impl TaxRates for $ty {
            fn with_rates(mut self, rates: Vec<TaxRate>) -> Self {
                self.tax_rates = Some(rates);
                self
            }
        }
    )+};
}
impl_tax_rates!(TariffEnergy, TariffTime, TariffFixed);

/// One dimension of an OCPP tariff: its price list in element order, and the
/// one VAT rate every price in it is quoted against.
struct Priced<P> {
    prices: Vec<P>,
    tax: Option<TaxRate>,
}

/// One dimension's price list, in element order, and the VAT rate the whole
/// list is quoted against.
///
/// `None` when the tariff does not price the dimension anywhere — which is not
/// the same as pricing it at zero, and OCPP reads an absent element the way
/// OCPI does: there is no cost for it.
fn prices<P: DimensionPrice>(
    tariff: &Tariff,
    dimension: Dimension,
    crossing: &mut Crossing<()>,
) -> Result<Option<Priced<P>>, SeamError> {
    let carrying: Vec<(usize, &TariffElement)> = tariff
        .elements
        .iter()
        .enumerate()
        .filter(|(_, element)| element.component(dimension).is_some())
        .collect();
    if carrying.is_empty() {
        return Ok(None);
    }

    // One `taxRates` list per dimension, so every price under it is quoted
    // against one rate. A tiered tariff whose tiers sit in different VAT
    // categories has two taxable amounts under one heading, and there is
    // no field to say so: carrying the first rate would tax the second tier at
    // the first tier's rate, in a document the station computes its own totals
    // from.
    let rates: Vec<Option<Decimal>> = carrying
        .iter()
        .filter_map(|(_, element)| element.component(dimension))
        .map(|component| component.vat)
        .collect();
    let vat = rates[0];
    if rates.iter().any(|rate| *rate != vat) {
        return Err(SeamError::TariffNotCarriedByOcpp {
            pointer: format!("/{}/taxRates", P::pointer(dimension)),
            reason: format!(
                "this tariff charges {dimension} at more than one VAT rate and OCPP 2.1 carries \
                 one rate list per dimension: the second rate would be dropped and its prices \
                 taxed at the first"
            ),
        });
    }

    let mut out = Vec::with_capacity(carrying.len());
    for (index, element) in carrying {
        // Unwrapped above by the filter.
        let Some(component) = element.component(dimension) else {
            continue;
        };
        let pointer = format!("/{}/prices/{}", P::pointer(dimension), out.len());

        // A block size is meaningless for a flat fee — the rating ignores it —
        // so noting it there would report a difference that does not exist.
        if component.step_size > 1 && dimension != Dimension::Flat {
            crossing.note(
                &pointer,
                format!(
                    "this component bills {dimension} in blocks of {} {} and OCPP 2.1 has no field \
                     for it: the station will show and total the quantity it measured, and the CDR \
                     this workspace issues will round up to the block. OCPI 3.0 removes the field \
                     and advises setting it to 1",
                    component.step_size,
                    match dimension {
                        Dimension::Energy => "Wh",
                        _ => "s",
                    }
                ),
            );
        }

        let quoted = P::quote(dimension, component.price)?;
        let net = net_of(quoted, tariff.tax_included, vat, &pointer)?;
        let (wire, carried) = to_wire(net).ok_or_else(|| SeamError::UnrepresentableDecimal {
            pointer: pointer.clone(),
            value: net.to_string(),
        })?;
        if carried != net {
            let rate = vat.unwrap_or(Decimal::ZERO);
            crossing.note(
                &pointer,
                format!(
                    "OCPP 2.1 quotes prices excluding tax and carries at most eighteen decimals: \
                     {quoted} at {rate} % is {net}, which the wire states as {carried}. A station \
                     grossing that back up gets {}, not {quoted}",
                    carried * (Decimal::ONE + rate / HUNDRED)
                ),
            );
        }

        out.push(P::price(wire, &element.restrictions, index)?);
    }

    // A tariff quoted with no tax regime states no rate; one quoted net or
    // gross states the rate the station grosses up with.
    let tax = match (tariff.tax_included, vat) {
        (TaxIncluded::NotApplicable, _) | (_, None) => None,
        (TaxIncluded::Yes | TaxIncluded::No, Some(rate)) => {
            let pointer = format!("/{}/taxRates/0/tax", P::pointer(dimension));
            let (wire, carried) =
                to_wire(rate).ok_or_else(|| SeamError::UnrepresentableDecimal {
                    pointer: pointer.clone(),
                    value: rate.to_string(),
                })?;
            if carried != rate {
                crossing.note(
                    &pointer,
                    format!("a VAT rate of {rate} % is stated as {carried} % on the wire"),
                );
            }
            // `stack` 0: on the net price. A compound tax needs a second entry
            // and this model has one rate per component to give it.
            Some(TaxRate::new("VAT", wire).with_stack(0))
        }
    };

    Ok(Some(Priced { prices: out, tax }))
}

/// The net figure OCPP quotes, from the price the tariff states.
///
/// `[OCPP 2.1 Part 2, TariffEnergyPrice]` — "Price per kWh (**excl. tax**)" —
/// and the same for `priceMinute` and `priceFixed`. A gross tariff's net is
/// `gross / (1 + rate/100)`, which is not generally a terminating decimal: the
/// crossing carries it at the wire's own precision and notes the residual the
/// station will re-derive.
fn net_of(
    quoted: Decimal,
    basis: TaxIncluded,
    vat: Option<Decimal>,
    pointer: &str,
) -> Result<Decimal, SeamError> {
    match basis {
        // Already the figure OCPP asks for.
        TaxIncluded::No | TaxIncluded::NotApplicable => Ok(quoted),
        TaxIncluded::Yes => {
            // A component stating no rate is a component at **zero** per cent,
            // which is what `Rated::tax_summary` already reads it as: a gross
            // price with no tax in it is its own net. Refusing here instead
            // would be a second, stricter reading of one field.
            let rate = vat.unwrap_or(Decimal::ZERO);
            let factor = Decimal::ONE + rate / HUNDRED;
            if factor.is_zero() {
                return Err(SeamError::TariffNotCarriedByOcpp {
                    pointer: pointer.to_owned(),
                    reason: "a VAT rate of exactly −100 % makes the gross-to-net factor zero: no \
                             net price grosses up to a non-zero amount at it"
                        .to_owned(),
                });
            }
            Ok(quoted / factor)
        }
    }
}

/// One dimension's OCPP price type, and the unit it is quoted in.
trait DimensionPrice: Sized {
    /// The field of `TariffType` this list lives under, for a note's pointer.
    ///
    /// A function of the dimension rather than a constant, because the two time
    /// dimensions share one price type and land in **different** fields —
    /// `chargingTime` and `idleTime` — and a note pointing at the wrong one
    /// names a field the reader does not have open, which is the whole of what
    /// a pointer is for.
    fn pointer(dimension: Dimension) -> &'static str;

    /// The tariff's stored price in the unit this wire field is quoted in.
    ///
    /// # Errors
    ///
    /// [`SeamError::TariffNotCarriedByOcpp`] where the conversion is not exact.
    fn quote(dimension: Dimension, price: Decimal) -> Result<Decimal, SeamError>;

    /// One price with its conditions.
    ///
    /// # Errors
    ///
    /// [`SeamError::TariffNotCarriedByOcpp`] where OCPP has no field for a
    /// condition the element states — which is not the same set for every
    /// dimension.
    ///
    /// `element` is the element's position, and it is here for the one
    /// implementation that can refuse: a fixed fee's conditions carry only the
    /// wall clock, so its diagnostic has to name which element's fee it is
    /// about. The two that cannot refuse ignore it.
    fn price(price: Wire, restrictions: &Restrictions, element: usize) -> Result<Self, SeamError>;
}

impl DimensionPrice for TariffEnergyPrice {
    fn pointer(_: Dimension) -> &'static str {
        "energy"
    }

    fn quote(_: Dimension, price: Decimal) -> Result<Decimal, SeamError> {
        // Both sides quote per kWh.
        Ok(price)
    }

    fn price(price: Wire, restrictions: &Restrictions, _element: usize) -> Result<Self, SeamError> {
        Ok(Self::new(price).with_conditions(conditions(restrictions)?))
    }
}

impl DimensionPrice for TariffTimePrice {
    fn pointer(dimension: Dimension) -> &'static str {
        match dimension {
            Dimension::ParkingTime => "idleTime",
            _ => "chargingTime",
        }
    }

    /// # The unit that makes a lawful tariff unrepresentable
    ///
    /// OCPI carries a time price per **hour** and OCPP 2.1 carries one per
    /// **minute**, which is the unit `[AFIR Art. 5(4)]` states the duty in.
    /// Sixty has a factor of three, so an ordinary occupancy fee of €2.50 an
    /// hour is €0.041666… a minute and has no exact decimal spelling at all.
    ///
    /// `emob-tariff` already reports that as an AFIR breach for an ad-hoc
    /// tariff — a price a driver cannot be shown exactly is not one "known to
    /// end users before they initiate". Here it is not advice: the field is per
    /// minute, and writing a rounded figure into it makes the station charge a
    /// price the tariff does not. The remedy is in the message and it is the
    /// same one: quote an hourly rate divisible by three.
    fn quote(dimension: Dimension, price: Decimal) -> Result<Decimal, SeamError> {
        price_per_minute(price).ok_or(SeamError::TariffNotCarriedByOcpp {
            pointer: format!("/{}/prices", Self::pointer(dimension)),
            reason: format!(
                "{price} per hour has no exact price per minute ({price} / 60 does not \
                 terminate), and OCPP 2.1 quotes time by the minute. A rounded figure is a price \
                 the station charges and the tariff does not. Quote an hourly rate divisible by \
                 three"
            ),
        })
    }

    fn price(price: Wire, restrictions: &Restrictions, _element: usize) -> Result<Self, SeamError> {
        Ok(Self::new(price).with_conditions(conditions(restrictions)?))
    }
}

impl DimensionPrice for TariffFixedPrice {
    fn pointer(_: Dimension) -> &'static str {
        "fixedFee"
    }

    fn quote(_: Dimension, price: Decimal) -> Result<Decimal, SeamError> {
        Ok(price)
    }

    fn price(price: Wire, restrictions: &Restrictions, element: usize) -> Result<Self, SeamError> {
        Ok(Self::new(price).with_conditions(fixed_conditions(restrictions, element)?))
    }
}

/// Carry an element's restrictions onto the conditions an energy or time price
/// takes.
///
/// # Errors
///
/// [`SeamError::UnrepresentableDecimal`] for a threshold outside what the wire
/// can carry — a duration above OCPP's 32-bit seconds, above all.
fn conditions(restrictions: &Restrictions) -> Result<TariffConditions, SeamError> {
    let clock = |t: time::Time| format!("{:02}:{:02}", t.hour(), t.minute());
    let date = |d: time::Date| format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day());

    let mut full = TariffConditions::new();
    full.start_time_of_day = restrictions.start_time.map(clock);
    full.end_time_of_day = restrictions.end_time.map(clock);
    full.valid_from_date = restrictions.start_date.map(date);
    full.valid_to_date = restrictions.end_date.map(date);
    full.day_of_week = (!restrictions.days_of_week.is_empty())
        .then(|| restrictions.days_of_week.iter().copied().map(day).collect());
    // OCPP counts energy in Wh and power in W where OCPI counts kWh and kW.
    // A factor of a thousand is exact in decimal and moves the point rather
    // than dividing, so a threshold keeps every digit it was written with.
    full.min_energy = restrictions
        .min_kwh
        .map(|kwh| wire_of(kwh * THOUSAND))
        .transpose()?;
    full.max_energy = restrictions
        .max_kwh
        .map(|kwh| wire_of(kwh * THOUSAND))
        .transpose()?;
    full.min_power = restrictions
        .min_power_kw
        .map(|kw| wire_of(kw * THOUSAND))
        .transpose()?;
    full.max_power = restrictions
        .max_power_kw
        .map(|kw| wire_of(kw * THOUSAND))
        .transpose()?;
    // `minTime`/`maxTime` are the duration of the whole transaction — charging
    // and idle together — which is what `min_duration_s` is read against here
    // and what OCPI means by it. `minChargingTime`/`minIdleTime` are narrower
    // questions this model does not ask.
    full.min_time = restrictions.min_duration_s.map(seconds).transpose()?;
    full.max_time = restrictions.max_duration_s.map(seconds).transpose()?;

    Ok(full)
}

/// The same restrictions, in the narrower shape a **fixed fee** takes.
///
/// # Why the flat fee is the strict one
///
/// `TariffConditionsFixed` carries the wall clock and nothing else — no energy,
/// no power, no duration `[OCPP 2.1 Part 2, TariffConditionsFixed]`, which
/// follows from a fixed fee being "evaluated only at the start of a
/// transaction": at that instant no energy has flowed and no time has passed,
/// so there is nothing for such a condition to be read against.
///
/// A session fee that applies only above 20 kWh is therefore a fee OCPP 2.1
/// cannot condition, and one published without its condition is not narrower,
/// it is **wider**: the station charges it on every session. That is the same
/// inversion the OCPI crossing refuses an unevaluable restriction over, met on
/// a different field.
///
/// # Errors
///
/// [`SeamError::TariffNotCarriedByOcpp`] for a fee restricted on a quantity.
fn fixed_conditions(
    restrictions: &Restrictions,
    element: usize,
) -> Result<TariffConditionsFixed, SeamError> {
    let quantity_bound = restrictions.min_kwh.is_some()
        || restrictions.max_kwh.is_some()
        || restrictions.min_power_kw.is_some()
        || restrictions.max_power_kw.is_some()
        || restrictions.min_duration_s.is_some()
        || restrictions.max_duration_s.is_some();
    if quantity_bound {
        return Err(SeamError::TariffNotCarriedByOcpp {
            pointer: format!("/fixedFee/prices/{element}/conditions"),
            reason:
                "this session fee applies only under an energy, power or duration condition, and \
                 OCPP 2.1's conditions for a fixed fee carry only the wall clock \
                 [OCPP 2.1 Part 2, TariffConditionsFixed]. Published without the condition the fee \
                 is not narrower but wider: the station would charge it on every session"
                    .to_owned(),
        });
    }

    let full = conditions(restrictions)?;
    let mut fixed = TariffConditionsFixed::new();
    fixed.start_time_of_day = full.start_time_of_day;
    fixed.end_time_of_day = full.end_time_of_day;
    fixed.valid_from_date = full.valid_from_date;
    fixed.valid_to_date = full.valid_to_date;
    fixed.day_of_week = full.day_of_week;
    Ok(fixed)
}

/// A price bound, in the two figures OCPP's `PriceType` states one in.
///
/// The same rule the OCPI crossing applies to `PriceLimit`: the pre-tax figure
/// is the one a receiver enforces, so a gross bound is converted at the rate the
/// tariff's own components carry — [`Tariff::vat_basis`], asked through the same
/// function both crossings and the rating engine use.
///
/// A basis **nobody stated** is zero per cent here, exactly as
/// [`emob_tariff::Rated::tax_summary`] already reads one, so an ordinary gross
/// price list with a `minCost` and no VAT rate anywhere publishes with
/// `exclTax == inclTax` instead of being refused. Only components that state
/// *different* rates leave no single taxable amount, and only that refuses.
fn cost(
    amount: Decimal,
    tariff: &Tariff,
    pointer: &str,
    crossing: &mut Crossing<()>,
) -> Result<Price, SeamError> {
    let mut price = Price::new();
    match tariff.tax_included {
        TaxIncluded::No | TaxIncluded::NotApplicable => {
            price.excl_tax = Some(wire_of(amount)?);
        }
        TaxIncluded::Yes => {
            let Some(rate) = tariff.vat_basis().rate() else {
                return Err(SeamError::TariffNotCarriedByOcpp {
                    pointer: pointer.to_owned(),
                    reason: "this bound is gross and the tariff's components carry more than one \
                             VAT rate, so no single taxable amount corresponds to it"
                        .to_owned(),
                });
            };
            let factor = Decimal::ONE + rate / HUNDRED;
            if factor.is_zero() {
                return Err(SeamError::TariffNotCarriedByOcpp {
                    pointer: pointer.to_owned(),
                    reason: "a VAT rate of exactly −100 % leaves no net amount this bound could \
                             be stated before tax"
                        .to_owned(),
                });
            }
            let net = emob_core::Money::new(amount / factor, tariff.currency)
                .round_to_minor_unit()
                .amount();
            if net * factor != amount {
                crossing.note(
                    format!("{pointer}/exclTax"),
                    format!(
                        "this tariff's prices are gross and OCPP states a bound before tax as well: \
                         {amount} at {rate} % is {net} to the minor unit, which grosses back up to \
                         {}",
                        net * factor
                    ),
                );
            }
            price.excl_tax = Some(wire_of(net)?);
            price.incl_tax = Some(wire_of(amount)?);
        }
    }
    Ok(price)
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

/// An instant in the spelling OCPP reads.
fn instant(at: time::OffsetDateTime) -> Result<ocpp_kit::types::DateTime, SeamError> {
    at.format(&time::format_description::well_known::Rfc3339)
        .ok()
        .and_then(|text| ocpp_kit::types::DateTime::parse(&text).ok())
        .ok_or_else(|| SeamError::UnrepresentableDecimal {
            pointer: "/validFrom".to_owned(),
            value: at.to_string(),
        })
}

/// A duration threshold in OCPP's 32-bit seconds.
fn seconds(value: u64) -> Result<i32, SeamError> {
    i32::try_from(value).map_err(|_| SeamError::UnrepresentableDecimal {
        pointer: "/conditions/minTime".to_owned(),
        value: value.to_string(),
    })
}

/// A decimal the wire carries exactly, or a refusal naming it.
fn wire_of(value: Decimal) -> Result<Wire, SeamError> {
    match to_wire(value) {
        Some((wire, carried)) if carried == value => Ok(wire),
        _ => Err(SeamError::UnrepresentableDecimal {
            pointer: String::new(),
            value: value.to_string(),
        }),
    }
}

/// The widest-scale wire decimal that fits, and the exact value it denotes.
///
/// `ocpp-kit`'s `Decimal` is a 64-bit mantissa at a scale of at most eighteen —
/// the JSON number OCPP-J actually carries, kept exact rather than rounded
/// through an `f64`. A `rust_decimal` is 96 bits at a scale of at most
/// twenty-eight, so a value can be outside it, and every quotient this module
/// takes is at the wider type's full precision.
///
/// The scale is walked **down** from what the value carries, so a figure that
/// already fits crosses untouched — scale is a statement the tariff made and a
/// conversion has no business weakening one it did not have to. The second
/// return is what the wire value actually says, which is how the caller knows
/// whether it has something to report.
fn to_wire(value: Decimal) -> Option<(Wire, Decimal)> {
    use rust_decimal::RoundingStrategy;

    let start = value.scale().min(u32::from(Wire::MAX_SCALE));
    for dp in (0..=start).rev() {
        let rounded = value.round_dp_with_strategy(dp, RoundingStrategy::MidpointAwayFromZero);
        let (Ok(mantissa), Ok(scale)) = (
            i64::try_from(rounded.mantissa()),
            u8::try_from(rounded.scale()),
        ) else {
            continue;
        };
        if scale <= Wire::MAX_SCALE {
            return Some((Wire::new(mantissa, scale), rounded));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_core::Currency;
    use emob_tariff::{
        Chargeable, PriceComponent, TariffElement, TariffKind, rate as rate_session,
    };
    use rust_decimal::prelude::FromStr;
    use time::macros::datetime;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap()
    }

    fn at() -> time::OffsetDateTime {
        datetime!(2026-01-02 10:00 +1)
    }

    fn gross(components: Vec<PriceComponent>) -> Tariff {
        Tariff::simple(
            "ad-hoc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            emob_core::TimeZone::new("Europe/Berlin").unwrap(),
            components,
        )
    }

    #[test]
    fn the_price_the_station_shows_is_the_price_that_rates() {
        // A net tariff crosses untouched: OCPP quotes prices excluding tax and
        // this one already does.
        let mut tariff = gross(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
        ]);
        tariff.tax_included = TaxIncluded::No;

        let crossing = to_ocpp(&tariff, at()).unwrap();
        let energy = crossing.value.energy.as_ref().unwrap();
        assert_eq!(energy.prices[0].price_kwh.to_string(), "0.49");
        assert_eq!(energy.tax_rates.as_ref().unwrap()[0].tax.to_string(), "19");
        assert_eq!(energy.tax_rates.as_ref().unwrap()[0].stack, Some(0));
        assert!(crossing.is_lossless(), "{:?}", crossing.notes());
    }

    #[test]
    fn the_driver_facing_description_is_the_afir_disclosure() {
        // OCPP 2.1 requires the station to show `description`, and this is the
        // one `[AFIR Art. 5(4)]` asks for — derived from the tariff that rates,
        // in the tariff's own basis, per kWh before per session.
        let tariff = gross(vec![
            PriceComponent::new(Dimension::Flat, dec("0.50")),
            PriceComponent::new(Dimension::Energy, dec("0.49")),
        ]);
        let crossing = to_ocpp(&tariff, at()).unwrap();
        let description = crossing.value.description.as_ref().unwrap();
        assert_eq!(description.len(), 1);
        assert_eq!(
            description[0].content,
            "0.49 EUR / kWh · 0.50 EUR / session"
        );
    }

    #[test]
    fn an_hourly_fee_with_no_exact_price_per_minute_is_refused() {
        // The defect `emob-tariff` reports as an AFIR breach, met on the wire
        // that states the duty's own unit. €2.50 an hour is €0.041666… a
        // minute, and a rounded figure is a price the station charges and the
        // tariff does not.
        let mut tariff = gross(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")),
            PriceComponent::new(Dimension::ParkingTime, dec("2.50")),
        ]);
        tariff.tax_included = TaxIncluded::No;

        let err = to_ocpp(&tariff, at()).unwrap_err();
        assert!(
            err.to_string().contains("divisible by three"),
            "the remedy belongs in the message: {err}"
        );

        // …and the rate beside it that does divide crosses exactly.
        let mut lawful = tariff;
        lawful.elements[0].components[1] = PriceComponent::new(Dimension::ParkingTime, dec("6.00"));
        let crossing = to_ocpp(&lawful, at()).unwrap();
        assert_eq!(
            crossing.value.idle_time.as_ref().unwrap().prices[0]
                .price_minute
                .to_string(),
            "0.10"
        );
        assert!(crossing.value.charging_time.is_none(), "no charging fee");
    }

    #[test]
    fn a_gross_price_crosses_net_and_the_residual_is_stated() {
        // €0.49 including 19 % has no exact net: 0.49 / 1.19 does not
        // terminate. The wire carries it at its own precision and the note says
        // what the station will gross it back up to.
        let tariff = gross(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
        ]);
        let crossing = to_ocpp(&tariff, at()).unwrap();
        let net = &crossing.value.energy.as_ref().unwrap().prices[0].price_kwh;
        assert!(net.to_string().starts_with("0.41176470588235294"), "{net}");
        assert!(
            crossing.reasons().any(|r| r.contains("/energy/prices/0")),
            "{:?}",
            crossing.notes()
        );
    }

    #[test]
    fn a_tier_in_a_second_vat_category_is_refused_rather_than_taxed_at_the_first() {
        // OCPP carries one `taxRates` list per dimension, so a tiered tariff
        // whose tiers sit in different tax categories would have the second
        // rate simply dropped and its prices taxed at the first.
        let tariff = Tariff {
            elements: vec![
                TariffElement {
                    components: vec![
                        PriceComponent::new(Dimension::Energy, dec("0.39")).with_vat(dec("19")),
                    ],
                    restrictions: Restrictions {
                        max_kwh: Some(dec("10")),
                        ..Restrictions::default()
                    },
                },
                TariffElement::unrestricted(vec![
                    PriceComponent::new(Dimension::Energy, dec("0.59")).with_vat(dec("7")),
                ]),
            ],
            tax_included: TaxIncluded::No,
            ..gross(vec![])
        };
        let err = to_ocpp(&tariff, at()).unwrap_err();
        assert!(err.to_string().contains("more than one VAT rate"), "{err}");
    }

    #[test]
    fn a_session_fee_ocpp_cannot_condition_is_refused_rather_than_widened() {
        // `TariffConditionsFixed` carries the wall clock and nothing else, so a
        // fee that applies above 20 kWh would be charged on every session.
        let tariff = Tariff {
            elements: vec![TariffElement {
                components: vec![PriceComponent::new(Dimension::Flat, dec("0.50"))],
                restrictions: Restrictions {
                    min_kwh: Some(dec("20")),
                    ..Restrictions::default()
                },
            }],
            tax_included: TaxIncluded::No,
            ..gross(vec![])
        };
        let err = to_ocpp(&tariff, at()).unwrap_err();
        assert!(err.to_string().contains("wider"), "{err}");
    }

    #[test]
    fn a_tiered_energy_price_keeps_the_order_that_selects_it() {
        // OCPI picks the first element with a component for the dimension whose
        // restrictions match; OCPP picks the first price in the dimension's own
        // list whose conditions match. Projecting one onto the other in element
        // order makes those the same choice.
        let tariff = Tariff {
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
            tax_included: TaxIncluded::No,
            ..gross(vec![])
        };
        let crossing = to_ocpp(&tariff, at()).unwrap();
        let prices = &crossing.value.energy.as_ref().unwrap().prices;
        assert_eq!(prices.len(), 2);
        assert_eq!(prices[0].price_kwh.to_string(), "0.39");
        // 10 kWh is 10 000 Wh: OCPP counts energy in watt-hours.
        assert_eq!(
            prices[0]
                .conditions
                .as_ref()
                .unwrap()
                .max_energy
                .unwrap()
                .to_string(),
            "10000"
        );
        assert_eq!(prices[1].price_kwh.to_string(), "0.59");
        assert!(prices[1].conditions.as_ref().unwrap().max_energy.is_none());
    }

    #[test]
    fn a_block_size_and_an_expiry_are_noted_because_ocpp_has_no_field_for_either() {
        let mut tariff = gross(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_step_size(1000),
        ]);
        tariff.tax_included = TaxIncluded::No;
        tariff.valid_until = Some(datetime!(2026-02-01 00:00 UTC));

        let crossing = to_ocpp(&tariff, at()).unwrap();
        assert!(crossing.reasons().any(|r| r.contains("blocks of 1000 Wh")));
        assert!(crossing.reasons().any(|r| r.contains("clear it")));
    }

    #[test]
    fn an_unevaluable_restriction_is_refused_the_same_way_the_ocpi_crossing_refuses_it() {
        let mut tariff = gross(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        tariff.tax_included = TaxIncluded::No;
        tariff.elements[0]
            .restrictions
            .unevaluable
            .push("reservation".to_owned());

        let err = to_ocpp(&tariff, at()).unwrap_err();
        assert!(err.to_string().contains("widens"), "{err}");
    }

    #[test]
    fn a_note_points_at_the_field_the_reader_has_open() {
        // The two time dimensions share one price type and land in different
        // fields. A note about the occupancy fee that points at `/chargingTime`
        // names a field the reader does not have, which is the whole of what a
        // pointer is for.
        let mut tariff = gross(vec![
            PriceComponent::new(Dimension::ParkingTime, dec("6.00")).with_step_size(300),
        ]);
        tariff.tax_included = TaxIncluded::No;

        let crossing = to_ocpp(&tariff, at()).unwrap();
        assert_eq!(crossing.notes().len(), 1, "{:?}", crossing.notes());
        assert_eq!(crossing.notes()[0].pointer, "/idleTime/prices/0");

        // …and a fee OCPP quotes per minute that this tariff quotes per hour is
        // refused under the field it would have gone in.
        let mut unquotable = tariff;
        unquotable.elements[0].components[0].price = dec("2.50");
        let err = to_ocpp(&unquotable, at()).unwrap_err();
        assert!(err.to_string().contains("/idleTime/prices"), "{err}");
    }

    #[test]
    fn a_flat_fee_block_size_is_not_reported_because_nothing_rounds_to_it() {
        // `apply_step` ignores `step_size` for a flat fee, so a note claiming
        // the CDR rounds up to the block would report a difference that does
        // not exist.
        let mut tariff = gross(vec![
            PriceComponent::new(Dimension::Flat, dec("0.50")).with_step_size(10),
        ]);
        tariff.tax_included = TaxIncluded::No;
        assert!(to_ocpp(&tariff, at()).unwrap().is_lossless());
    }

    #[test]
    fn the_finished_document_is_checked_against_ocpp_s_own_schema() {
        // Every bound above is enumerated from the specification, and an
        // enumeration is a list somebody maintains. The kit owns the schema, so
        // the last question is its to answer — and a tariff id too long for the
        // field is refused before a station ever sees it.
        let mut tariff = gross(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        tariff.tax_included = TaxIncluded::No;
        tariff.id = "x".repeat(61).parse().unwrap();

        let err = to_ocpp(&tariff, at()).unwrap_err();
        assert!(err.to_string().contains("schema"), "{err}");
        assert!(err.to_string().contains("tariffId"), "{err}");
    }

    #[test]
    fn a_tariff_deeper_than_the_description_list_says_what_the_driver_will_not_see() {
        // OCPP's description holds ten lines. A note rather than a refusal:
        // nothing about what is *charged* changes, and refusing would leave the
        // station showing no price at all — a worse breach of the same article
        // than showing ten tiers of twelve.
        let elements = (0..12)
            .map(|i| TariffElement {
                components: vec![PriceComponent::new(
                    Dimension::Energy,
                    dec(&format!("0.{:02}", 40 + i)),
                )],
                restrictions: Restrictions {
                    max_kwh: Some(Decimal::from(i + 1)),
                    ..Restrictions::default()
                },
            })
            .collect();
        let tariff = Tariff {
            elements,
            tax_included: TaxIncluded::No,
            ..gross(vec![])
        };

        let crossing = to_ocpp(&tariff, at()).unwrap();
        assert_eq!(crossing.value.description.as_ref().unwrap().len(), 10);
        assert!(
            crossing
                .reasons()
                .any(|r| r.contains("/description") && r.contains("AFIR Art. 5(4)")),
            "{:?}",
            crossing.notes()
        );
        // …and all twelve prices still cross, because the prices are the thing
        // that has to be right.
        assert_eq!(crossing.value.energy.as_ref().unwrap().prices.len(), 12);
    }

    #[test]
    fn the_running_cost_is_the_gross_the_invoice_will_state() {
        let mut tariff = gross(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
        ]);
        tariff.tax_included = TaxIncluded::No;

        let session = Chargeable::energy_only(
            emob_core::Energy::from_kwh(dec("29.500")).unwrap(),
            at(),
            at() + time::Duration::minutes(30),
        )
        .unwrap();
        let rated = rate_session(&tariff, &session);

        // 29.5 × 0.49 = 14.455 net, grossed at 19 % and rounded per VAT
        // category the way EN 16931 states an invoice: 17.20145 → 17.20.
        assert_eq!(rated.gross().to_string(), "17.20 EUR");
        assert_eq!(cost_updated(&rated).unwrap().to_string(), "17.20");
    }

    #[test]
    fn a_gross_bound_is_stated_before_tax_and_after_it() {
        // OCPP states a `Price` in both bases and the station enforces the
        // pre-tax one, so a gross bound is converted at the tariff's own rate.
        let mut tariff = gross(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
        ]);
        tariff.min_price = Some(dec("5.00"));

        let crossing = to_ocpp(&tariff, at()).unwrap();
        let min = crossing.value.min_cost.as_ref().unwrap();
        // 5.00 / 1.19 is 4.2016806…, so 4.20 to the minor unit, and the note
        // says the station will gross that back up to 4.998 rather than 5.00.
        assert_eq!(min.excl_tax.unwrap().to_string(), "4.20");
        assert_eq!(min.incl_tax.unwrap().to_string(), "5.00");
        assert!(
            crossing.reasons().any(|r| r.contains("/minCost/exclTax")),
            "{:?}",
            crossing.notes()
        );
    }

    #[test]
    fn a_gross_bound_on_a_tariff_that_states_no_rate_is_carried_rather_than_refused() {
        // The defect this pair of tests exists for. "No component states a
        // rate" and "the components state different rates" were one `None`, and
        // this crossing refused the first with the second's reason — so an
        // ordinary gross price list with a minimum charge and no VAT rate
        // anywhere could not be installed on a 2.1 station at all.
        //
        // Nothing is being stripped out, so the two bases are the same figure —
        // which is exactly what the rating engine computes for the session that
        // bound applies to.
        let mut tariff = gross(vec![PriceComponent::new(Dimension::Energy, dec("0.49"))]);
        tariff.min_price = Some(dec("5.00"));

        let crossing = to_ocpp(&tariff, at()).unwrap();
        let min = crossing.value.min_cost.as_ref().unwrap();
        assert_eq!(min.excl_tax.unwrap().to_string(), "5.00");
        assert_eq!(min.incl_tax.unwrap().to_string(), "5.00");
        assert!(
            !crossing.reasons().any(|r| r.contains("minCost")),
            "nothing was approximated, so there is nothing to report: {:?}",
            crossing.notes()
        );
    }

    #[test]
    fn a_gross_bound_on_a_tariff_mixing_rates_is_still_refused() {
        // The case that genuinely has no answer: two taxable amounts under one
        // bound, and OCPP's `Price` holds one.
        let mut tariff = gross(vec![
            PriceComponent::new(Dimension::Energy, dec("0.49")).with_vat(dec("19")),
            PriceComponent::new(Dimension::Flat, dec("0.50")).with_vat(dec("7")),
        ]);
        tariff.max_price = Some(dec("40.00"));

        let err = to_ocpp(&tariff, at()).unwrap_err();
        assert!(
            matches!(
                &err,
                SeamError::TariffNotCarriedByOcpp { pointer, .. } if pointer == "/maxCost"
            ),
            "{err}"
        );
    }

    #[test]
    fn the_wire_keeps_the_scale_the_tariff_wrote() {
        // Scale is a statement about a price — 0.49 and 0.490 are two different
        // prices to show a driver — so a value that fits crosses untouched.
        let (wire, carried) = to_wire(dec("0.490")).unwrap();
        assert_eq!(wire.to_string(), "0.490");
        assert_eq!(carried, dec("0.490"));

        // …and one wider than the wire is narrowed to what it can hold, with
        // the caller told what it actually says.
        let third = Decimal::ONE / Decimal::from(3);
        let (wire, carried) = to_wire(third).unwrap();
        assert_eq!(wire.scale(), 18);
        assert_ne!(carried, third);
        assert_eq!(carried.to_string(), wire.to_string());
    }
}
