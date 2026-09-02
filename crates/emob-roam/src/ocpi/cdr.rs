//! The canonical record, carried onto OCPI — and back.
//!
//! # Three places OCPI cannot hold what this workspace measured
//!
//! **A duration in hours is usually not a decimal.** OCPI types `total_time`,
//! `TIME` and `PARKING_TIME` as a number of **hours**
//! `[OCPI 2.3.0 §mod_cdrs_cdr_object]`. Sixty has a factor of three and 3600
//! has two of them, so a session of twenty minutes is `1/3` of an hour and has
//! no exact decimal spelling at all. Twenty-one minutes does — `0.35` — and
//! twenty-two does not. This is the same arithmetic that makes an occupancy
//! fee of €2.50 an hour unlawful under `[AFIR Art. 5(4)]`, met again one layer
//! out, and the resolution is the same: state the exact figure where the
//! format allows one, and where it does not, say so rather than let a rounded
//! number pass for the measurement.
//!
//! The consequence is concrete. The money on this record was computed from
//! **whole seconds**, multiplied before it was divided; a partner who
//! re-derives it from the rounded `total_time` gets a different number, and
//! the difference has no explanation anywhere in the document. So the crossing
//! records one.
//!
//! **A charging period has a start and no end.** OCPI gives a period only
//! `start_date_time`; a reader takes each period as running to the next one's
//! start, and the last as running to `end_date_time`. A canonical
//! [`emob_cdr::ChargingPeriod`] has both ends, because a
//! station that authorises at 10:00 and sends its first meter value at 10:20
//! produces a record that must not claim twenty minutes of measurement that
//! never happened. Crossing that record into a model with no ends hands the
//! receiver a document in which those twenty minutes are inside a measured
//! period. Nothing is invented to fill the hole — inventing a zero-energy
//! period would assert that no energy moved, which is precisely what nobody
//! measured — and the span is reported instead.
//!
//! **Energy has no direction.** `ENERGY_EXPORT` exists and the specification
//! marks it *Session Only*, so a CDR has only `ENERGY`, and `total_energy`
//! carries no sign convention. A V2G discharge crossing to OCPI would arrive
//! as an ordinary draw and settle backwards. That one is not a note; it is
//! [`RoamError::ExportNotExpressible`].

use emob_cdr::{Cdr, ChargingPeriod, EvidenceRef};
use emob_core::{Direction, Energy};
use emob_session::{AuthPath, Provenance};
use emob_tariff::{Dimension, Rated};
use ocpi_kit::types::{CiString, Number, OcpiString};
use ocpi_kit::v2_3_0::cdrs::{
    AuthMethod, CdrDimension, CdrDimensionType, CdrLocation, CdrToken, SignedData, SignedValue,
};
use ocpi_kit::v2_3_0::{Price, TaxAmount};
use rust_decimal::Decimal;

use crate::crossing::{AbsorbLossy as _, Crossing};
use crate::error::RoamError;
use crate::ocpi::location::{bounded, bounded_ocpi};
use crate::partner::Partner;
use crate::token::RoamingToken;

/// The decimal places a duration in hours is rounded to when it has no exact
/// spelling.
///
/// Four is what OCPI's own examples carry. The figure matters less than the
/// fact that it is stated in one place and reported wherever it is used.
pub const HOURS_SCALE: u32 = 4;

/// Seconds in an hour, and the reason a duration in hours is usually not a
/// decimal: `3600 = 2⁴ · 3² · 5²`, and only the twos and fives divide out.
const SECONDS_PER_HOUR: i64 = 3600;

/// A duration in hours, exactly, when the format can hold it exactly.
///
/// `None` when it cannot, which is whenever the second count is not a multiple
/// of nine — the two factors of three in 3600 are the whole story.
#[must_use]
pub fn exact_hours(seconds: i64) -> Option<Decimal> {
    (seconds % 9 == 0).then(|| Decimal::from(seconds) / Decimal::from(SECONDS_PER_HOUR))
}

/// A duration in hours, rounded when it has to be, with whether it was.
///
/// The result is normalised, so half an hour goes out as `0.5` rather than
/// `0.50`. Trailing zeros are information *everywhere else in this workspace*
/// — a register reporting `2935.600 kWh` is stating three decimals of
/// resolution `[OCMF Tab. 7]` — and here they are not: the scale of a quotient
/// is an artefact of the division, and a duration in hours makes no claim
/// about the clock that measured it.
fn hours(seconds: i64) -> (Decimal, bool) {
    exact_hours(seconds).map_or_else(
        || {
            (
                (Decimal::from(seconds) / Decimal::from(SECONDS_PER_HOUR))
                    .round_dp(HOURS_SCALE)
                    .normalize(),
                true,
            )
        },
        |exact| (exact.normalize(), false),
    )
}

/// The signed records, as a partner's verifier needs them.
///
/// A canonical [`EvidenceRef`] names its records by digest, because a CDR
/// travels through roaming and a full OCMF blob per reading makes it enormous.
/// OCPI's `SignedData` wants the records themselves, so the payloads are
/// supplied here by whoever holds the evidence store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedPayload {
    /// What the record is — `Start`, `End`, or the OCMF pagination it carries.
    pub nature: String,
    /// The record verbatim, exactly as the meter signed it.
    ///
    /// Verbatim is not a nicety. The signature covers the bytes as written, so
    /// a payload that has been re-serialised on the way through does not
    /// verify at the far end, and the partner's only conclusion is that the
    /// operator tampered with it.
    pub signed_data: String,
    /// The human-readable rendering OCPI asks to accompany it.
    pub plain_data: String,
}

/// Everything the wire needs that a canonical CDR deliberately does not carry.
#[derive(Debug, Clone)]
pub struct Context<'a> {
    /// The token the session was authorised with — see [`crate::token`] for
    /// why it is an argument rather than a field.
    pub token: &'a RoamingToken,
    /// Where the session happened, from the register that publishes it.
    pub location: CdrLocation,
    /// The signed records, when the evidence store could supply them.
    pub signed: Vec<SignedPayload>,
    /// The public key the records were checked against, as the registry holds
    /// it.
    ///
    /// A **claim**, and OCPI's own field for it says as much. A key that
    /// arrives beside the record it signs proves only that whoever sent both
    /// owns a private key, which is why the key that decides anything here
    /// comes from the key registry, out of band. It is carried so the partner
    /// can compare it with theirs and find out *which* of the two documents is
    /// the surprise.
    pub public_key: Option<String>,
    /// When this document was written.
    ///
    /// An argument, because a domain crate reads no clock: an export replayed
    /// two years later has to produce the same bytes.
    pub last_updated: time::OffsetDateTime,
}

/// Carry a canonical CDR onto OCPI 2.3.0.
///
/// # Errors
///
/// [`RoamError::NotRated`] for a record with no price, because OCPI's
/// `total_cost` is required and zero means free;
/// [`RoamError::ExportNotExpressible`] for a V2G discharge, which OCPI has no
/// way to distinguish from a draw; [`RoamError::NoPeriods`] for a record with
/// nothing behind its total; [`RoamError::SignedDataRequired`] when the
/// partner settles on signed metering data and none was supplied; and
/// [`RoamError::TooLong`] or [`RoamError::InvalidString`] for a value that
/// does not fit the shape OCPI bounds it to.
#[allow(clippy::too_many_lines)]
pub fn to_ocpi(
    cdr: &Cdr,
    partner: &Partner,
    context: &Context<'_>,
) -> Result<Crossing<ocpi_kit::v2_3_0::Cdr>, RoamError> {
    if cdr.direction == Direction::Export {
        return Err(RoamError::ExportNotExpressible {
            energy: cdr.total_energy.to_string(),
            direction: cdr.direction,
        });
    }
    if cdr.periods.is_empty() {
        return Err(RoamError::NoPeriods);
    }
    let cost = cdr.cost.as_ref().ok_or(RoamError::NotRated)?;
    if partner.requires_signed_data && context.signed.is_empty() {
        return Err(RoamError::SignedDataRequired {
            partner: partner.party.clone(),
        });
    }

    let mut crossing = Crossing::lossless(());

    let (periods, charging_seconds, parking_seconds) = crossing.absorb_from(charging_periods(cdr));

    let session_seconds = (cdr.ended_at - cdr.started_at).whole_seconds();
    let (total_time, time_rounded) = hours(session_seconds);
    if time_rounded {
        crossing.note(
            "/total_time",
            format!(
                "{session_seconds} s is {total_time} h rounded to {HOURS_SCALE} places: an hour \
                 has 3600 seconds and 3600 has two factors of three, so most durations have no \
                 exact decimal in hours [OCPI 2.3.0 §mod_cdrs_cdr_object]. The cost beside it \
                 was computed from whole seconds, so re-deriving it from this figure will not \
                 reproduce it"
            ),
        );
    }

    let (total_parking_time, parking_rounded) = hours(parking_seconds);
    if parking_rounded {
        crossing.note(
            "/total_parking_time",
            format!("{parking_seconds} s is {total_parking_time} h rounded, for the same reason"),
        );
    }

    // The periods cover what the meter measured; the record covers the
    // session. Where the two differ, an OCPI reader — which takes a period as
    // running to the next one's start — puts the difference *inside* a
    // measured period.
    let measured_seconds = charging_seconds + parking_seconds;
    if measured_seconds < session_seconds {
        let uncovered = session_seconds - measured_seconds;
        crossing.note(
            "/charging_periods",
            format!(
                "{uncovered} s of this session are covered by no charging period, because the \
                 meter did not measure them. An OCPI period has a start and no end, so a reader \
                 attributes that time to whichever period precedes it — this record does not \
                 claim it, and nothing was invented to fill it. The specification's own \
                 `total_charging_time = total_time - total_parking_time` therefore counts those \
                 {uncovered} s as charging, which no meter reading supports \
                 [OCPI 2.3.0 §mod_cdrs_cdr_object]"
            ),
        );
    }

    let auth_method = auth_method(cdr.auth_path);
    if let Some(reason) = auth_method_note(cdr.auth_path) {
        crossing.note("/auth_method", reason);
    }

    let signed_data = signed_data(cdr.evidence.as_ref(), context, &mut crossing)?;
    let currency = cost.rated.currency;

    // The parts are each rounded to the currency's minor unit and the whole is
    // rounded once, so with several dimensions in play they can differ by a
    // minor unit. `total_cost` is the number that settles; the breakdown is
    // what makes it checkable, and a partner comparing the two deserves to
    // know which is which before they open a dispute about a cent.
    let parts: Decimal = [
        Dimension::Energy,
        Dimension::Time,
        Dimension::ParkingTime,
        Dimension::Flat,
    ]
    .into_iter()
    .filter_map(|d| component_price(&cost.rated, d))
    .map(|p| p.after_taxes().get())
    .sum();
    let whole = cost.rated.gross().amount();
    if parts != whole {
        crossing.note(
            "/total_cost",
            format!(
                "the per-dimension costs come to {parts} and `total_cost` says {whole}. Each \
                 part is rounded to the minor unit of {currency} and the whole is rounded once, \
                 which is the same reason an EN 16931 invoice's total is the sum of what it \
                 shows. `total_cost` is the figure that settles"
            ),
        );
    }

    if let Some(previous) = &cdr.supersedes {
        crossing.note(
            "/credit_reference_id",
            format!(
                "this record supersedes {previous}. OCPI corrects with a Credit CDR that \
                 reverses the original and a replacement beside it \
                 [OCPI 2.3.0 §mod_cdrs_cdr_object]; this is the replacement, and the reversal \
                 is a separate document the partner has to have received"
            ),
        );
    }

    // 39 is the bound on the member and the specification narrows it: *"Normal
    // (non-credit) CDRs SHALL only have an ID with a maximum length of 36"*,
    // the extra three being room to append to a credit. So the narrower bound
    // is the one this is checked against, and the wider one is the field.
    let _: CiString<{ ocpi_kit::v2_3_0::cdrs::NON_CREDIT_ID_MAX_LEN }> =
        bounded("id", cdr.key.id.as_str())?;

    let built = ocpi_kit::v2_3_0::Cdr::builder()
        .country_code(bounded::<2>("country_code", cdr.key.party.country_code())?)
        .party_id(bounded::<3>("party_id", cdr.key.party.party_id())?)
        .id(bounded::<39>("id", cdr.key.id.as_str())?)
        .start_date_time(cdr.started_at)
        .end_date_time(cdr.ended_at)
        .session_id(bounded::<36>("session_id", cdr.session_id.as_str())?)
        .cdr_token(cdr_token(context.token)?)
        .auth_method(auth_method)
        .cdr_location(context.location.clone())
        .currency(bounded_ocpi::<3>("currency", cost.rated.currency.as_str())?)
        .charging_periods(periods)
        .maybe_signed_data(signed_data)
        .total_cost(price(&cost.rated))
        // OCPI breaks the total out per dimension and most implementations
        // fill only `total_cost`, which leaves the receiver unable to check
        // any part of it against its own tariff. The lines are already there.
        .maybe_total_fixed_cost(component_price(&cost.rated, Dimension::Flat))
        .total_energy(Number::new(cdr.total_energy.kwh()))
        .maybe_total_energy_cost(component_price(&cost.rated, Dimension::Energy))
        .total_time(Number::new(total_time))
        .maybe_total_time_cost(component_price(&cost.rated, Dimension::Time))
        .total_parking_time(Number::new(total_parking_time))
        .maybe_total_parking_cost(component_price(&cost.rated, Dimension::ParkingTime))
        .maybe_credit_reference_id(
            cdr.supersedes
                .as_ref()
                .map(|previous| bounded::<39>("credit_reference_id", previous.id.as_str()))
                .transpose()?,
        )
        .last_updated(context.last_updated)
        .build();

    Ok(crossing.map(|()| built))
}

/// The charging periods, and how many seconds of the session they cover,
/// split by whether the vehicle was charging.
#[allow(clippy::type_complexity)]
fn charging_periods(
    cdr: &Cdr,
) -> Crossing<(Vec<ocpi_kit::v2_3_0::cdrs::ChargingPeriod>, i64, i64)> {
    let mut crossing = Crossing::lossless(());
    let mut periods = Vec::with_capacity(cdr.periods.len());
    let (mut charging_seconds, mut parking_seconds) = (0_i64, 0_i64);

    for (index, period) in cdr.periods.iter().enumerate() {
        let seconds = period.duration().whole_seconds();
        let (in_hours, rounded) = hours(seconds);
        if rounded {
            crossing.note(
                format!("/charging_periods/{index}/dimensions"),
                format!("{seconds} s is {in_hours} h rounded to {HOURS_SCALE} places"),
            );
        }
        if period.charging {
            charging_seconds += seconds;
        } else {
            parking_seconds += seconds;
        }

        // A period that was interpolated is a period whose energy is an
        // assumption of constant power across a gap, which a tapering charge
        // curve does not deliver. The assumption travels with the number all
        // the way to the partner's copy, because a settlement dispute turns on
        // it and OCPI has no field that says so.
        if period.provenance != Provenance::Measured {
            crossing.note(
                format!("/charging_periods/{index}"),
                format!(
                    "this period's {} was interpolated between two readings, not measured. \
                     OCPI has no field for the difference, and it is what a dispute about this \
                     quarter hour turns on",
                    period.energy
                ),
            );
        }

        periods.push(period_of(period, in_hours));
    }

    // The gap an OCPI reader cannot see: between one period's end and the
    // next one's start there is time this record does not claim, and a reader
    // computing spans from starts alone puts it inside the earlier period.
    for pair in cdr.periods.windows(2) {
        let gap = (pair[1].start - pair[0].end).whole_seconds();
        if gap > 0 {
            crossing.note(
                "/charging_periods",
                format!(
                    "{gap} s between {} and {} were not measured. An OCPI period has no end, so \
                     a reader will attribute them to the earlier period",
                    pair[0].end, pair[1].start
                ),
            );
        }
    }

    crossing.map(|()| (periods, charging_seconds, parking_seconds))
}

/// One period, in OCPI's dimensions.
fn period_of(period: &ChargingPeriod, in_hours: Decimal) -> ocpi_kit::v2_3_0::cdrs::ChargingPeriod {
    // `TIME` is *"Time charging"* and `PARKING_TIME` is *"Time not charging"*,
    // so which one a period carries is the same fact the CDR states rather
    // than infers — a quarter hour that genuinely measured `0.000 kWh` while
    // the session was charging is a taper, not an occupancy, and pricing it as
    // the latter charges a driver for parking they were told was charging.
    let time_dimension = if period.charging {
        CdrDimensionType::Time
    } else {
        CdrDimensionType::ParkingTime
    };

    ocpi_kit::v2_3_0::cdrs::ChargingPeriod::builder()
        .start_date_time(period.start)
        .dimensions(vec![
            CdrDimension::new(CdrDimensionType::Energy, Number::new(period.energy.kwh())),
            CdrDimension::new(time_dimension, Number::new(in_hours)),
        ])
        .build()
}

/// The token, in OCPI's shape.
fn cdr_token(token: &RoamingToken) -> Result<CdrToken, RoamError> {
    Ok(CdrToken::builder()
        .country_code(bounded::<2>(
            "cdr_token.country_code",
            token.issuer.country_code(),
        )?)
        .party_id(bounded::<3>("cdr_token.party_id", token.issuer.party_id())?)
        .uid(bounded::<36>("cdr_token.uid", &token.uid)?)
        .token_type(ocpi_kit::v2_3_0::tokens::TokenType::from(token.token_type))
        .contract_id(bounded::<36>(
            "cdr_token.contract_id",
            token.contract_id.as_str(),
        )?)
        .build())
}

/// How the session was authorised, in the three values OCPI has.
#[must_use]
pub fn auth_method(path: AuthPath) -> AuthMethod {
    match path {
        // The CPO decided, from a list it already held. An ad-hoc session is
        // the same shape: nothing went out to a provider.
        AuthPath::LocalList | AuthPath::AdHoc => AuthMethod::Whitelist,
        AuthPath::RemoteCommand => AuthMethod::Command,
        AuthPath::Roaming | AuthPath::PlugAndCharge | AuthPath::AutoCharge => {
            AuthMethod::AuthRequest
        }
        // `AuthPath` is `#[non_exhaustive]`. A path this build of the crossing
        // has not learned yet reports that somebody was asked, which is the
        // claim that does not put the decision on the CPO's own list — and
        // `auth_method_note` says the path had no exact spelling.
        _ => AuthMethod::AuthRequest,
    }
}

/// What the three values cannot say about six paths.
fn auth_method_note(path: AuthPath) -> Option<String> {
    match path {
        AuthPath::PlugAndCharge | AuthPath::AutoCharge => Some(format!(
            "this session was authorised by {}, and OCPI's `auth_method` has one value — \
             AUTH_REQUEST — for both Plug & Charge and AutoCharge \
             [OCPI 2.3.0 §mod_cdrs_authmethod_enum]. The first is a contract certificate the \
             vehicle presented; the second is a MAC address, which is not a standard, not \
             authenticated and trivially spoofable. The distinction survives on this record in \
             the identification strength read off the signed meter data, and nowhere else",
            match path {
                AuthPath::PlugAndCharge => "Plug & Charge",
                _ => "AutoCharge",
            }
        )),
        AuthPath::AdHoc => Some(
            "this was an ad-hoc session: no contract, and no authorization request went to any \
             provider. OCPI's nearest value is WHITELIST, which says the CPO decided — true, \
             but it does not say there was nobody to ask. The token type says AD_HOC_USER"
                .to_owned(),
        ),
        AuthPath::LocalList | AuthPath::Roaming | AuthPath::RemoteCommand => None,
        other => Some(format!(
            "this session was authorised by {other:?}, which this build of the crossing has no \
             OCPI spelling for. It went out as AUTH_REQUEST, which is the claim that does not \
             put the decision on the operator's own list"
        )),
    }
}

/// The signed records, when there are any to send.
fn signed_data(
    evidence: Option<&EvidenceRef>,
    context: &Context<'_>,
    crossing: &mut Crossing<()>,
) -> Result<Option<SignedData>, RoamError> {
    let Some(evidence) = evidence else {
        crossing.note(
            "/signed_data",
            "this CDR rests on no signed meter records. It may be perfectly good telemetry and \
             it may not be the basis of an energy invoice in Germany [MessEG §33]",
        );
        return Ok(None);
    };
    if context.signed.is_empty() {
        crossing.note(
            "/signed_data",
            format!(
                "the record names {} signed meter payload(s) by digest and the evidence store \
                 supplied none, so the partner cannot repeat the check [MessEG §33] — the \
                 digests are on this side and the payloads are not on the wire",
                evidence.payload_digests.len()
            ),
        );
        return Ok(None);
    }

    let mut values = Vec::with_capacity(context.signed.len());
    for payload in &context.signed {
        values.push(SignedValue {
            nature: bounded::<32>("signed_data.nature", &payload.nature)?,
            plain_data: bounded_ocpi::<5000>("signed_data.plain_data", &payload.plain_data)?,
            // Not bounds-checked, deliberately, and `ocpi-kit` carries it the
            // same way: the signature covers these bytes, an OCMF record from
            // a real meter routinely runs past the `string(5000)` the
            // specification gives, and a record that has been shortened to fit
            // is a record that does not verify. `validate()` reports the
            // length; nothing repairs it.
            signed_data: ocpi_kit::types::OcpiString::new_lenient(&payload.signed_data),
            extensions: ocpi_kit::types::Extensions::new(),
        });
    }

    let public_key = context
        .public_key
        .as_deref()
        .map(|key| bounded_ocpi::<512>("signed_data.public_key", key))
        .transpose()?;
    if public_key.is_some() {
        crossing.note(
            "/signed_data/public_key",
            "the key beside the records is a claim, not a binding. A key arriving on the same \
             wire as the record it signs proves only that whoever sent both owns a private key; \
             check these against the key your own registry holds, out of band",
        );
    }

    Ok(Some(
        SignedData::builder()
            .encoding_method(bounded::<36>(
                "signed_data.encoding_method",
                &evidence.encoding_method,
            )?)
            .maybe_public_key(public_key)
            .signed_values(values)
            .build(),
    ))
}

/// A rated total as an OCPI price, with the VAT breakdown EN 16931 needs.
fn price(rated: &Rated) -> Price {
    let taxes: Vec<TaxAmount> = rated
        .tax_summary()
        .into_iter()
        .filter(|line| !line.tax.is_zero())
        .filter_map(|line| {
            TaxAmount::new("VAT", Some(Number::new(line.rate)), Number::new(line.tax)).ok()
        })
        .collect();

    let mut price = Price::new(Number::new(rated.net().amount()));
    price.taxes = taxes;
    price
}

/// One dimension's share of the total, as an OCPI price.
///
/// OCPI breaks the total out per dimension and most implementations fill only
/// `total_cost`, which leaves the receiver able to disagree with the whole
/// number and with no part of it. The lines are already there; carrying them
/// costs nothing and makes the record checkable a piece at a time.
///
/// # Why this rounds, and rounds where the rating rounds
///
/// A gross tariff states what the driver pays and the net has to be recovered
/// by dividing by `1 + rate/100`. At 19 % that divisor is `1.19`, and `8.82 /
/// 1.19` is `7.4117647058823529411764705882…` — a figure with more significant
/// digits than a JSON number survives, which `ocpi-kit`'s own validator
/// reports as `Imprecise` rather than letting a peer's parser decide where to
/// cut it.
///
/// So each side is rounded to the **currency's own minor unit**, which is
/// exactly what [`Rated::tax_summary`] does for the record's total — the yen
/// has no minor unit and the dinar has three, and a component rounded to a
/// hard-coded two invents a hundredth of a unit that does not exist. Rounding
/// the two sides separately and taking the tax as the remainder keeps
/// `before_taxes + tax` equal to the amount, which is the identity a receiver
/// checks.
fn component_price(rated: &Rated, dimension: Dimension) -> Option<Price> {
    // Per VAT rate, not per dimension. One dimension can be charged at two
    // prices — that is what a tier is — and the two tiers can sit in different
    // tax categories. Reading the rate off the first line and applying it to
    // the summed amount taxes the second tier at the first tier's rate, which
    // is a number the partner's own accountant will not reproduce.
    let taxes = rated.tax_summary_for(dimension);
    if taxes.is_empty() {
        return None;
    }

    let net: Decimal = taxes.iter().map(|line| line.net).sum();
    let mut price = Price::new(Number::new(net));
    price.taxes = taxes
        .iter()
        .filter(|line| !line.tax.is_zero())
        .filter_map(|line| {
            TaxAmount::new("VAT", Some(Number::new(line.rate)), Number::new(line.tax)).ok()
        })
        .collect();
    Some(price)
}

/// The total energy of an OCPI CDR, checked against its own periods.
///
/// The first question to ask of a record somebody else built, and the one a
/// canonical [`Cdr`] answers by construction. A partner whose periods do not
/// sum to their own total is a partner whose re-rating will not match theirs.
///
/// # Errors
///
/// [`RoamError::DoesNotConserve`] when the two disagree.
pub fn check_conserves(cdr: &ocpi_kit::v2_3_0::Cdr) -> Result<Energy, RoamError> {
    // Both spellings of *energy drawn*. `ENERGY` is the only one this crate
    // writes, but this function's purpose is records somebody else built, and a
    // partner who used `ENERGY_IMPORT` would otherwise sum to nothing here.
    // `inbound::preflight` reports the deviation itself.
    let sum: Decimal = cdr
        .charging_periods
        .iter()
        .filter_map(|period| {
            period
                .volume(CdrDimensionType::Energy)
                .or_else(|| period.volume(CdrDimensionType::EnergyImport))
        })
        .map(Number::get)
        .sum();

    if sum != cdr.total_energy.get() {
        return Err(RoamError::DoesNotConserve {
            sum: sum.to_string(),
            total: cdr.total_energy.get().to_string(),
        });
    }
    Energy::from_kwh(sum).map_err(|_| RoamError::DoesNotConserve {
        sum: sum.to_string(),
        total: cdr.total_energy.get().to_string(),
    })
}

/// The signed records an inbound CDR carries, for verification **here**.
///
/// The mirror of the transparency file's reader, and for the same reason: a
/// record is checked against the key this side's registry holds, never against
/// the key the document carries, which is the artefact under examination.
/// So this returns the payloads and does not verify them — verifying is
/// `emob-eichrecht`'s job and it needs a registry this crate has no business
/// holding.
#[must_use]
pub fn inbound_payloads(cdr: &ocpi_kit::v2_3_0::Cdr) -> Vec<SignedPayload> {
    cdr.signed_data
        .as_ref()
        .map(|data| {
            data.signed_values
                .iter()
                .map(|value| SignedPayload {
                    nature: value.nature.to_string(),
                    signed_data: value.signed_data.as_str().to_owned(),
                    plain_data: value.plain_data.as_str().to_owned(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The key an inbound CDR claims its records were signed with.
///
/// Worth exactly one comparison: a key that differs from the one the registry
/// holds is a dispute with an answer, and one that matches narrows the
/// argument to the numbers. It is never what the records are verified against.
#[must_use]
pub fn claimed_key(cdr: &ocpi_kit::v2_3_0::Cdr) -> Option<&OcpiString<512>> {
    cdr.signed_data.as_ref()?.public_key.as_ref()
}

/// A CDR's id, as the ledger keys it.
#[must_use]
pub fn key_of(cdr: &ocpi_kit::v2_3_0::Cdr) -> Option<emob_cdr::CdrKey> {
    Some(emob_cdr::CdrKey {
        party: emob_core::PartyId::new(cdr.country_code.as_str(), cdr.party_id.as_str()).ok()?,
        id: cdr.id.as_str().parse().ok()?,
    })
}

/// Carry a CDR onto a partner's older OCPI version, folding the downgrade's
/// own loss report into the crossing's.
///
/// A partner on 2.2.1 is the ordinary case, not the exceptional one. What is
/// not ordinary is handing them a document with two half-accounts of what it
/// cost to reach them.
#[must_use]
pub fn downgrade(
    crossing: Crossing<ocpi_kit::v2_3_0::Cdr>,
) -> Crossing<ocpi_kit::v2_2_1::cdrs::Cdr> {
    use ocpi_kit::convert::Downgrade;

    let mut carried = Crossing::lossless(());
    for note in crossing.notes() {
        carried.note(note.pointer.clone(), note.reason.clone());
    }
    let converted = crossing.value.downgrade();
    carried.absorb("", &converted.lossy);
    carried.map(|()| converted.value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duration_in_hours_is_exact_exactly_when_nine_divides_the_seconds() {
        // An hour has 3600 seconds and 3600 = 2⁴·3²·5². The twos and fives
        // divide out; the threes are what make most durations non-terminating.
        assert_eq!(exact_hours(3600), Some(Decimal::ONE));
        assert_eq!(
            exact_hours(1260).map(|h| h.to_string()),
            Some("0.35".into())
        ); // 21 min
        assert_eq!(exact_hours(1200), None, "20 minutes is a third of an hour");
        assert_eq!(exact_hours(1320), None, "22 minutes"); // 1320 = 8·165
        assert_eq!(exact_hours(900).map(|h| h.to_string()), Some("0.25".into())); // a quarter hour
    }

    #[test]
    fn the_quarter_hour_grid_this_workspace_settles_on_is_always_exact() {
        // Not a coincidence worth relying on silently: 900 = 4·225 and
        // 9 | 900, so every whole number of quarter hours has an exact
        // spelling in hours. It is the sessions that start and stop between
        // them that do not.
        for quarters in 1..=96_i64 {
            assert!(
                exact_hours(quarters * 900).is_some(),
                "{quarters} quarter hours"
            );
        }
    }

    #[test]
    fn a_rounded_duration_says_it_was_rounded() {
        let (value, rounded) = hours(1200);
        assert!(rounded);
        assert_eq!(value.to_string(), "0.3333");

        let (value, rounded) = hours(1260);
        assert!(!rounded);
        assert_eq!(value.to_string(), "0.35");
    }

    #[test]
    fn ocpi_has_one_auth_method_for_two_things_that_must_not_be_conflated() {
        assert_eq!(
            auth_method(AuthPath::PlugAndCharge),
            auth_method(AuthPath::AutoCharge)
        );
        assert!(auth_method_note(AuthPath::PlugAndCharge).is_some());
        assert!(auth_method_note(AuthPath::AutoCharge).is_some());
        assert!(
            auth_method_note(AuthPath::Roaming).is_none(),
            "AUTH_REQUEST says exactly what a roaming authorisation was"
        );
    }
}
