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

use emob_cdr::{Cdr, ChargingPeriod, Cost, EvidenceRef};
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
use crate::partner::{OcpiVersion, Partner};
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

pub use crate::signed::SignedPayload;

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

    let (periods, parking_seconds) = crossing.absorb_from(charging_periods(cdr));

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
    //
    // Measured from the exact durations rather than from the whole-second
    // figures above: OCPP stamps events to the millisecond, each period's
    // `whole_seconds` truncates its own fraction, and a sum of truncations
    // falls short of the truncated span by up to a second per period — which
    // reported a gap on a record that had none.
    let covered: time::Duration = cdr.periods.iter().map(ChargingPeriod::duration).sum();
    let uncovered = ((cdr.ended_at - cdr.started_at) - covered).whole_seconds();
    if uncovered > 0 {
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

    // How much of the register was cable rather than vehicle
    // `[OCMF Tab. 7, CL]`. OCPI has no field for it and the compensation is
    // already inside `total_energy`, so nothing is adjusted — but a partner
    // disputing the energy will ask exactly this, and `[REA 6-A §3.2]` makes
    // telling the affected party what is inside a measured value a duty.
    if let Some(loss) = cdr.evidence.as_ref().and_then(|e| e.compensated_loss)
        && !loss.is_zero()
    {
        crossing.note(
            "/total_energy",
            format!(
                "{loss} of this session's register is cable loss the meter compensated \
                 [OCMF Tab. 7, CL]. It is already inside `total_energy` — nothing here is \
                 adjusted — and OCPI has no field to say so, which is the figure a dispute about \
                 this energy turns on [REA 6-A §3.2]"
            ),
        );
    }

    let signed_data = signed_data(cdr.evidence.as_ref(), context, &mut crossing)?;
    let currency = cost.rated.currency;

    // The parts are each rounded to the currency's minor unit and the whole is
    // rounded once, so with several dimensions in play they can differ by a
    // minor unit. `total_cost` is the number that settles; the breakdown is
    // what makes it checkable, and a partner comparing the two deserves to
    // know which is which before they open a dispute about a cent.
    // Five parts, not four. `total_reservation_cost` is one of the
    // per-dimension fields a receiver sums `[OCPI 2.3.0 §mod_cdrs_cdr_object]`,
    // and `total_cost` is the session **and** the reservation — so leaving it
    // out of both sides compared a four-part sum against a five-part total and
    // reported a discrepancy of exactly the reservation on every record that
    // carried one, quoting a `total_cost` the document does not state (D250).
    let parts: Decimal = [
        Dimension::Energy,
        Dimension::Time,
        Dimension::ParkingTime,
        Dimension::Flat,
    ]
    .into_iter()
    .filter_map(|d| component_price(&cost.rated, d))
    .chain(cost.reservation.as_ref().map(price))
    .map(|p| p.after_taxes().get())
    .sum();
    let whole = cost.gross().amount();
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

    price_notes(cost, &mut crossing);

    if let Some(previous) = &cdr.supersedes {
        // OCPI corrects a CDR in two documents: a Credit CDR that reverses the
        // original — `credit = true`, `credit_reference_id` naming it, and
        // `total_cost` negated — and then "a new CDR with a new unique ID and
        // the fields `credit` and `credit_reference_id` **omitted**"
        // `[OCPI 2.3.0 §mod_cdrs_cdr_object]`. This is that replacement, so
        // the link to what it replaces has no field to travel in; naming the
        // original in `credit_reference_id` would make this document claim to
        // be the reversal, which `ocpi-kit`'s own validator refuses. The
        // reversal is `to_ocpi_credit`, and the partner has to have received
        // it first.
        crossing.note(
            "/id",
            format!(
                "this record supersedes {previous}. OCPI carries that in two documents — a \
                 Credit CDR reversing the original, then this replacement with `credit` and \
                 `credit_reference_id` omitted [OCPI 2.3.0 §mod_cdrs_cdr_object] — so the \
                 replacement itself names nothing. Send the Credit CDR from `to_ocpi_credit` \
                 before it"
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
        // *"Reference to the authorization given by the eMSP"* — the provider's
        // own handle on the decision it answered the `Authorize` with. Without
        // it a partner settling this record has only the session id the CPO
        // invented, and nothing of its own to correlate against.
        .maybe_authorization_reference(
            cdr.authorization_reference
                .as_deref()
                .map(|reference| bounded::<36>("authorization_reference", reference))
                .transpose()?,
        )
        .cdr_location(context.location.clone())
        .currency(bounded_ocpi::<3>("currency", cost.rated.currency.as_str())?)
        .charging_periods(periods)
        .maybe_signed_data(signed_data)
        .total_cost(total_price(cost))
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
        // The reservation, in the field `[OCPI 2.3.0 §mod_cdrs_cdr_object]`
        // keeps for it. It is priced over a window that ran before any energy
        // moved, so it is its own rating and its own total — and it is inside
        // `total_cost`, which is why the pre-flight's own sum of the parts
        // reconciles only when this field is sent with the rest.
        .maybe_total_reservation_cost(cost.reservation.as_ref().map(price))
        .last_updated(context.last_updated)
        .build();

    Ok(crossing.map(|()| built))
}

/// The Credit CDR that reverses a record already sent to a partner.
///
/// OCPI has no way to amend a CDR: *"a CDR is immutable"*, and a correction is
/// two documents. The first is this one — the original again, with `credit`
/// set, `credit_reference_id` naming the original's `id`, and **only**
/// `total_cost` carrying "the negative amounts of the original CDR"
/// `[OCPI 2.3.0 §mod_cdrs_cdr_object]`. The second is the replacement, which
/// [`to_ocpi`] produces from the superseding record with both fields omitted.
///
/// `credit_id` is the Credit CDR's own id, which the specification wants
/// distinct from the original's — "the id of the original CDR with something
/// appended like for example `-C`" — and allows up to 39 characters for that
/// reason, three more than a normal record may use.
///
/// # Errors
///
/// Everything [`to_ocpi`] refuses, and [`RoamError::TooLong`] or
/// [`RoamError::InvalidString`] for a `credit_id` OCPI's 39-character field
/// will not hold.
pub fn to_ocpi_credit(
    original: &Cdr,
    partner: &Partner,
    context: &Context<'_>,
    credit_id: &str,
) -> Result<Crossing<ocpi_kit::v2_3_0::Cdr>, RoamError> {
    let outbound = to_ocpi(original, partner, context)?;
    let mut crossing = Crossing::lossless(());
    crossing.absorb_notes("", outbound.notes().to_vec());
    let mut credit = outbound.into_value_discarding_notes();

    credit.id = bounded::<39>("id", credit_id)?;
    credit.credit = Some(true);
    credit.credit_reference_id = Some(bounded::<39>(
        "credit_reference_id",
        original.key.id.as_str(),
    )?);

    // The specification is explicit that the reversal lives in `total_cost`
    // alone: every other total — energy, time, the per-dimension costs — is
    // carried as the original stated it, and a partner reconciling the two
    // documents reads the pair as "this one, undone".
    let before = credit.total_cost.before_taxes.get();
    let mut reversed = Price::new(Number::new(-before));
    reversed.taxes = credit
        .total_cost
        .taxes
        .iter()
        .filter_map(|tax| {
            TaxAmount::new(
                tax.name.as_str(),
                tax.percentage,
                Number::new(-tax.amount.get()),
            )
            .ok()
        })
        .collect();
    credit.total_cost = reversed;

    crossing.note(
        "/total_cost",
        format!(
            "this is the Credit CDR for {}: `total_cost` carries the negative of the original \
             and, as the specification prescribes, nothing else is negated \
             [OCPI 2.3.0 §mod_cdrs_cdr_object]. The energy and the per-dimension costs are the \
             original's, for the partner to match the pair by",
            original.key
        ),
    );

    Ok(crossing.map(|()| credit))
}

/// The OCPI field a rating note is **about**, as a JSON Pointer into the
/// partner's own copy.
///
/// A note names a dimension; the partner is looking at that dimension's total.
fn pointer_for(dimension: Dimension) -> &'static str {
    match dimension {
        Dimension::Energy => "/total_energy_cost",
        Dimension::Time => "/total_time_cost",
        Dimension::ParkingTime => "/total_parking_cost",
        Dimension::Flat => "/total_fixed_cost",
    }
}

/// Carry the rating's own account of the price onto the wire.
///
/// # Why a note and not a field
///
/// [`emob_tariff::RatingNote`] is serialisable *because* it is meant to travel:
/// "a note that stays behind in the process that produced it is a note nobody
/// can invoke". It was staying behind. OCPI states a quantity and a cost per
/// dimension and has no field for the distance between them, so a partner
/// receiving `total_energy: 30` beside a `total_energy_cost` computed for twenty
/// kilowatt-hours — a promotional first tier, a night-only energy price, any
/// tariff whose energy element is conditional — sees a document that does not
/// multiply out and opens a dispute about a discount the operator gave on
/// purpose. The same is true in the other direction of a `step_size`, which
/// bills a block more than the record states.
///
/// [`emob_tariff::RatingNote::concerns_the_payer`] is already the question
/// "is this a term of the price, or a fault in a document the payer did not
/// write" — asked here for the party that settles, exactly as `emob-billing`
/// asks it for the party that pays (D260). The rest stay with the operator:
/// a partner cannot act on a fault in this side's tariff.
fn price_notes(cost: &emob_cdr::Cost, crossing: &mut Crossing<()>) {
    let mut carry = |pointer: &str, note: &emob_tariff::RatingNote| {
        crossing.note(pointer.to_owned(), note.to_string());
    };
    for note in &cost.rated.notes {
        if !note.concerns_the_payer() {
            continue;
        }
        match note {
            emob_tariff::RatingNote::Unpriced { dimension, .. }
            | emob_tariff::RatingNote::RoundedToBlock { dimension, .. }
            | emob_tariff::RatingNote::DurationBelowResolution { dimension, .. } => {
                carry(pointer_for(*dimension), note);
            }
            // Seconds no dimension may price, inside a `total_parking_time`
            // that counts them: the one payer-facing note that is about a
            // quantity rather than about a cost.
            _ => carry("/total_parking_time", note),
        }
    }
    // The reservation is its own rating over its own window, and the partner
    // reads it in its own field.
    for note in cost.reservation.iter().flat_map(|rated| &rated.notes) {
        if note.concerns_the_payer() {
            carry("/total_reservation_cost", note);
        }
    }
}

/// The charging periods, and how many whole seconds of them the vehicle was
/// parked rather than charging — the figure `total_parking_time` states.
fn charging_periods(cdr: &Cdr) -> Crossing<(Vec<ocpi_kit::v2_3_0::cdrs::ChargingPeriod>, i64)> {
    let mut crossing = Crossing::lossless(());
    let mut periods = Vec::with_capacity(cdr.periods.len());
    let mut parking_seconds = 0_i64;

    for (index, period) in cdr.periods.iter().enumerate() {
        let seconds = period.duration().whole_seconds();
        let (in_hours, rounded) = hours(seconds);
        if rounded {
            crossing.note(
                format!("/charging_periods/{index}/dimensions"),
                format!("{seconds} s is {in_hours} h rounded to {HOURS_SCALE} places"),
            );
        }
        // `total_parking_time` is defined on **energy transfer** — "the
        // duration of the charging session where the EV was not charging (no
        // energy was transferred between EVSE and EV)"
        // `[OCPI 2.3.0 §mod_cdrs_cdr_object]` — while the priced
        // `PARKING_TIME` *dimension* is defined on the vehicle's demand. The
        // two are the same figure until the operator withholds power, and a
        // stack that computes one from the other is wrong in one of them.
        if !period.activity.transfers_energy() {
            parking_seconds += seconds;
        }

        // A period that was interpolated is a period whose energy is an
        // assumption of constant power across a gap, which a tapering charge
        // curve does not deliver. The assumption travels with the number all
        // the way to the partner's copy, because a settlement dispute turns on
        // it and OCPI has no field that says so.
        if period.provenance == Provenance::Interpolated {
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

    crossing.map(|()| (periods, parking_seconds))
}

/// One period, in OCPI's dimensions.
fn period_of(period: &ChargingPeriod, in_hours: Decimal) -> ocpi_kit::v2_3_0::cdrs::ChargingPeriod {
    // `TIME` is *"Time charging"* and `PARKING_TIME` is *"Time in this
    // ChargingPeriod during which the **vehicle is not requesting power**"*
    // `[OCPI 2.3.0 §mod_cdrs_chargingperiod_class]`, so which one a period
    // carries is the same fact the CDR states rather than infers — a quarter
    // hour that genuinely measured `0.000 kWh` while the session was charging
    // is a taper, not an occupancy, and pricing it as the latter charges a
    // driver for parking they were told was charging.
    //
    // A period the *operator* withheld power in is neither, and it crosses as
    // neither: the specification's own erratum says a `PARKING_TIME` volume
    // there would expose the driver to "penalizing loitering fees … when the
    // EVSE is not offering energy to the vehicle while the vehicle is still
    // requesting power". So the period carries its energy and no time
    // dimension, which is a statement the partner can read, and the seconds
    // are still inside the `total_time` the record states.
    let mut dimensions = vec![CdrDimension::new(
        CdrDimensionType::Energy,
        Number::new(period.energy.kwh()),
    )];
    if let Some(dimension) = Dimension::pricing(period.activity) {
        let wire = match dimension {
            Dimension::ParkingTime => CdrDimensionType::ParkingTime,
            _ => CdrDimensionType::Time,
        };
        dimensions.push(CdrDimension::new(wire, Number::new(in_hours)));
    }

    ocpi_kit::v2_3_0::cdrs::ChargingPeriod::builder()
        .start_date_time(period.start)
        .dimensions(dimensions)
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

/// What the record comes to in total — the session **and** its reservation.
///
/// `total_cost` is "the total sum of all the costs of this transaction"
/// `[OCPI 2.3.0 §mod_cdrs_cdr_object]`, and the pre-flight on the other side
/// checks it against the sum of the per-dimension fields — one of which is
/// `total_reservation_cost`. A total taken from the session's rating alone
/// leaves a record that fails its own receiver's arithmetic by exactly the
/// reservation.
///
/// The tax amounts merge **per rate** rather than concatenating: a session fee
/// at 20 % and a reservation at 20 % are one taxable group on the document, and
/// two `TaxAmount` entries at one rate is a breakdown no accountant reproduces.
fn total_price(cost: &Cost) -> Price {
    let Some(reservation) = cost.reservation.as_ref() else {
        return price(&cost.rated);
    };

    let net = cost.rated.net().amount() + reservation.net().amount();
    let mut by_rate: Vec<(Decimal, Decimal)> = Vec::new();
    for line in cost
        .rated
        .tax_summary()
        .into_iter()
        .chain(reservation.tax_summary())
    {
        if line.tax.is_zero() {
            continue;
        }
        match by_rate.iter_mut().find(|(rate, _)| *rate == line.rate) {
            Some((_, total)) => *total += line.tax,
            None => by_rate.push((line.rate, line.tax)),
        }
    }
    by_rate.sort_by_key(|(rate, _)| *rate);

    let mut out = Price::new(Number::new(net));
    out.taxes = by_rate
        .into_iter()
        .filter_map(|(rate, tax)| {
            TaxAmount::new("VAT", Some(Number::new(rate)), Number::new(tax)).ok()
        })
        .collect();
    out
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

/// A CDR in the version the partner it is addressed to actually speaks.
///
/// # Why the choice is not the caller's
///
/// [`Partner::version`] is what the registry records a peer as speaking, and
/// nothing read it: [`to_ocpi`] produced 2.3.0 for every partner and
/// [`downgrade`] was a second step a caller had to remember. A registry field
/// that decides nothing is a rule that is not enforced (rule 1) — and the thing
/// it fails to prevent is sending a 2.3.0 document to a peer that parses 2.2.1,
/// which is the ordinary case in this market rather than the exceptional one
/// (D265).
///
/// So the version decides, once, here. The account of what the downgrade cost
/// travels with the document either way, folded into the same [`Crossing`] as
/// everything else the translation could not carry.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Outbound {
    /// The canonical version.
    V2_3_0(Box<ocpi_kit::v2_3_0::Cdr>),
    /// The version most of the market still runs.
    V2_2_1(Box<ocpi_kit::v2_2_1::cdrs::Cdr>),
}

impl Outbound {
    /// Which version this document is written in.
    #[must_use]
    pub const fn version(&self) -> OcpiVersion {
        match self {
            Self::V2_3_0(_) => OcpiVersion::V2_3_0,
            Self::V2_2_1(_) => OcpiVersion::V2_2_1,
        }
    }
}

/// Carry a canonical CDR onto the OCPI version **this partner** speaks.
///
/// The one entry point a sender needs: it applies every refusal [`to_ocpi`]
/// applies, and then the version the registry records for the recipient rather
/// than the one the caller happened to reach for.
///
/// # Errors
///
/// Everything [`to_ocpi`] refuses.
pub fn for_partner(
    cdr: &Cdr,
    partner: &Partner,
    context: &Context<'_>,
) -> Result<Crossing<Outbound>, RoamError> {
    let crossing = to_ocpi(cdr, partner, context)?;
    Ok(match partner.version {
        OcpiVersion::V2_3_0 => crossing.map(|value| Outbound::V2_3_0(Box::new(value))),
        OcpiVersion::V2_2_1 => downgrade(crossing).map(|value| Outbound::V2_2_1(Box::new(value))),
    })
}

/// Carry a CDR onto a partner's older OCPI version, folding the downgrade's
/// own loss report into the crossing's.
///
/// A partner on 2.2.1 is the ordinary case, not the exceptional one. What is
/// not ordinary is handing them a document with two half-accounts of what it
/// cost to reach them. [`for_partner`] is what chooses between this and
/// [`to_ocpi`]; this stays public because a caller that already knows the
/// version — a re-send, a test — should not have to build a `Partner` to say so.
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
    fn the_ratings_own_account_of_the_price_reaches_the_partner() {
        // `RatingNote` is serialisable *because* it is meant to travel, and the
        // OCPI crossing was not carrying it. A partner receiving 30 kWh beside a
        // cost computed for twenty — an ordinary promotional first tier — reads a
        // document that does not multiply out, and OCPI has no field that says
        // why. The note is that field (D260).
        let rated = emob_tariff::Rated {
            lines: Vec::new(),
            currency: emob_core::Currency::EUR,
            tax_included: emob_tariff::TaxIncluded::Yes,
            adjustment: None,
            notes: vec![
                emob_tariff::RatingNote::Unpriced {
                    dimension: Dimension::Energy,
                    at: time::OffsetDateTime::UNIX_EPOCH,
                    periods: 1,
                    base_quantity: Decimal::from(10),
                },
                emob_tariff::RatingNote::WithheldNotPriced {
                    seconds: Decimal::from(600),
                    periods: 1,
                },
                // …and a fault in this side's own tariff, which a partner
                // cannot act on and does not receive.
                emob_tariff::RatingNote::UnevaluableRestriction {
                    index: 0,
                    restrictions: vec!["min_current".into()],
                },
            ],
        };
        let cost = emob_cdr::Cost {
            tariff_id: "t".parse().unwrap(),
            tariff_fingerprint: emob_tariff::Tariff::simple(
                "t".parse().unwrap(),
                emob_core::Currency::EUR,
                emob_tariff::TariffKind::AdHoc,
                emob_core::TimeZone::utc(),
                Vec::new(),
            )
            .fingerprint(),
            rated,
            reservation: None,
        };

        let mut crossing = Crossing::lossless(());
        price_notes(&cost, &mut crossing);

        let pointers: Vec<&str> = crossing
            .notes()
            .iter()
            .map(|n| n.pointer.as_str())
            .collect();
        assert_eq!(pointers, ["/total_energy_cost", "/total_parking_time"]);
        assert!(
            crossing.notes()[0]
                .reason
                .contains("10 kWh was not charged"),
            "{:?}",
            crossing.notes()
        );
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
