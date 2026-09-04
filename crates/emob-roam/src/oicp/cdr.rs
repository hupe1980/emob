//! The canonical charge detail record onto OICP 2.3, and back.
//!
//! See the [module documentation](super) for the two things that make this
//! wire different from OCPI: the record carries **no money**, and it carries
//! **four** timestamps rather than two.

use emob_cdr::Cdr;
use emob_core::{Direction, Energy};
use emob_session::AuthPath;
use oicp_kit::cpo::{
    CalibrationLawVerificationInfo, ChargeDetailRecord, MeteringStatus, SignedMeteringValue,
};
use oicp_kit::types::{DateTime, Identification, Number, Text, Validate as _};
use rust_decimal::Decimal;

use crate::crossing::Crossing;
use crate::error::RoamError;
use crate::partner::Partner;
use crate::signed::SignedPayload;
use crate::token::RoamingToken;

/// What OICP bounds a signed meter record to `[OICP 2.3 §SignedMeteringValue]`.
const SIGNED_VALUE_LIMIT: usize = 3000;

/// The register at the two ends of a session.
///
/// # Why it is an argument rather than a field on the record
///
/// A canonical [`Cdr`] states the energy a session **delivered**; the register
/// readings it opened and closed at are the meter's, and they live in the
/// evidence store beside the signed records that vouch for them. OICP wants
/// both, and *defines* `ConsumedEnergy` as the difference — so supplying them
/// turns that definition into a check this crate can run before Hubject does.
///
/// Optional, because a record re-rated from a partner has no register of ours
/// behind it. Omitting them costs a note rather than the crossing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeterWindow {
    /// The reading the session opened at.
    pub start: Energy,
    /// …and the one it closed at.
    pub end: Energy,
}

/// Everything the wire needs that a canonical CDR deliberately does not carry.
#[derive(Debug, Clone)]
pub struct Context<'a> {
    /// The token the session was authorised with — see [`crate::token`] for
    /// why it is an argument rather than a field.
    pub token: &'a RoamingToken,
    /// The session identifier Hubject issued when it authorised the session.
    ///
    /// **The broker's, not ours.** OICP opens a session at `AuthorizeStart` and
    /// a CDR settles the session the broker opened; one carrying an id the
    /// broker never issued is refused with `SessionIsInvalid`, after the driver
    /// has gone. So the id comes from the authorisation that started this
    /// session rather than from the record, which never held one.
    pub session_id: String,
    /// The register at the two ends, when the evidence store can supply it.
    pub meter: Option<MeterWindow>,
    /// The signed records, when the evidence store could supply them.
    pub signed: Vec<SignedPayload>,
    /// The public key the records were checked against, as the registry holds
    /// it.
    ///
    /// A **claim**, as on the OCPI side: a key that arrives beside the record
    /// it signs proves only that whoever sent both owns a private key. It is
    /// carried so the partner can compare it with theirs.
    pub public_key: Option<String>,
    /// The pricing product this session is billed under.
    ///
    /// The **only** thing on an OICP CDR that says anything about money, and it
    /// says it by naming an agreement rather than by carrying a figure. See
    /// [`to_oicp`].
    pub product_id: Option<String>,
    /// The conformity-assessment id of the metering system, in the shape the
    /// certifying authority issues it — `PTB - X-X-XXXX : V1 : 01Jan2020`.
    pub calibration_certificate: Option<String>,
    /// Where a driver fetches the transparency file for this session.
    pub verification_url: Option<String>,
}

/// Carry a canonical CDR onto OICP 2.3.
///
/// # The money does not cross, and that is the headline note
///
/// OICP's charge detail record has no cost field of any kind. The provider
/// re-derives what it owes from the `PartnerProductID` and the pricing product
/// behind it, so the number this side computed — from the record's own periods,
/// through the one door every price goes through — is not the number that
/// settles. It is reported on every record rather than treated as an
/// exception, because it is how the protocol works.
///
/// # Errors
///
/// [`RoamError::ExportNotExpressible`] for a V2G discharge, because
/// `ConsumedEnergy` carries no sign and a discharge would settle backwards;
/// [`RoamError::AutoChargeNotExpressible`] for a MAC-address session, because
/// OICP's only vehicle-borne identification names ISO 15118;
/// [`RoamError::NoPeriods`] for a record with nothing behind its total;
/// [`RoamError::SignedDataRequired`] when the partner settles on signed
/// metering data and none was supplied; [`RoamError::SignedValueTooLong`] for a
/// record the wire cannot carry whole; [`RoamError::RegisterDisagrees`] when the
/// supplied readings do not produce the record's own total; and
/// [`RoamError::InvalidString`] or [`RoamError::TooLong`] for a value that does
/// not fit the shape OICP bounds it to.
pub fn to_oicp(
    cdr: &Cdr,
    partner: &Partner,
    context: &Context<'_>,
) -> Result<Crossing<ChargeDetailRecord>, RoamError> {
    if cdr.direction == Direction::Export {
        return Err(RoamError::ExportNotExpressible {
            energy: cdr.total_energy.to_string(),
            direction: cdr.direction,
        });
    }
    if cdr.periods.is_empty() {
        return Err(RoamError::NoPeriods);
    }
    if partner.requires_signed_data && context.signed.is_empty() {
        return Err(RoamError::SignedDataRequired {
            partner: partner.party.clone(),
        });
    }

    let mut crossing = Crossing::lossless(());

    let (charging_start, charging_end) = metered_window(cdr);
    account(cdr, context, charging_start, charging_end, &mut crossing);

    // ── The energy, and OICP's own defining equation ────────────────────────
    let consumed = Number::new(cdr.total_energy.kwh());
    let (meter_start, meter_end) = register(cdr, context, &mut crossing)?;

    // ── The identification ──────────────────────────────────────────────────
    let identification = identification(cdr.auth_path, context.token)?;

    // ── The signed records ──────────────────────────────────────────────────
    let signed = signed_values(&context.signed, &mut crossing)?;
    let calibration = calibration_info(cdr, context);

    let record = ChargeDetailRecord::builder()
        .session_id(
            context
                .session_id
                .parse()
                .map_err(|error| RoamError::NotOnTheWire {
                    field: "SessionID",
                    value: context.session_id.clone(),
                    detail: format!("{error}"),
                })?,
        )
        .evse_id(
            cdr.evse_id
                .canonical()
                .parse()
                .map_err(|error| RoamError::NotOnTheWire {
                    field: "EvseID",
                    value: cdr.evse_id.canonical().to_string(),
                    detail: format!("{error}"),
                })?,
        )
        .identification(identification)
        .session_start(DateTime::from_offset(cdr.started_at))
        .session_end(DateTime::from_offset(cdr.ended_at))
        .charging_start(DateTime::from_offset(charging_start))
        .charging_end(DateTime::from_offset(charging_end))
        .consumed_energy(consumed)
        .maybe_meter_value_start(meter_start)
        .maybe_meter_value_end(meter_end)
        .maybe_partner_product_id(
            context
                .product_id
                .as_deref()
                .map(text::<50>)
                .transpose()
                .map_err(|value| RoamError::TooLong {
                    field: "PartnerProductID",
                    len: value.len(),
                    max: 50,
                })?,
        )
        .maybe_signed_metering_values(signed)
        .maybe_calibration_law_verification_info(calibration)
        .build_unchecked();

    // The kit's own schema, before a broker sees the document. Hubject
    // validates a CDR on submission and an EMP validates it again at billing,
    // by which time the session is over and a rejected record is a written-off
    // sale — the same argument `emob-ocpp` makes for checking a tariff against
    // `ocpp-kit` before a station is given one.
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

/// Everything this crossing does **not** carry, said in the order a reader
/// meets it.
///
/// Extracted because the account is the point of the crossing rather than a
/// detail of it: the largest omission — the money — has to come first, before
/// the sub-second arithmetic, and a reader skimming it should not have to find
/// that among the builder calls.
fn account(
    cdr: &Cdr,
    context: &Context<'_>,
    charging_start: time::OffsetDateTime,
    charging_end: time::OffsetDateTime,
    crossing: &mut Crossing<()>,
) {
    // ── The money ───────────────────────────────────────────────────────────
    //
    // Said first because it is the largest thing this crossing does not carry,
    // and because a reader skimming the account has to meet it before the
    // sub-second arithmetic below.
    // `Cost::gross` rather than `cost.rated.total()`: a record's price is the
    // session **and** the reservation that preceded it, and the second figure
    // read the session alone — so a partner was told a record priced at €4.90
    // that the record itself says is €7.90 (D250, one wire further along). It is
    // also the basis a payer reads, where `total()` is whichever basis the
    // tariff happened to be written in.
    let priced = cdr
        .cost
        .as_ref()
        .map_or_else(|| "nothing".to_owned(), |cost| cost.gross().to_string());
    crossing.note(
        "/PartnerProductID",
        format!(
            "this record priced at {priced} on the issuing side and OICP carries no cost field of \
             any kind: the provider re-derives what it owes from the pricing product named here \
             `[OICP 2.3 §PricingProductData]`. The two figures agree only where both parties hold \
             the same version of that product, and nothing on this document lets either of them \
             find out that they do not"
        ),
    );
    if context.product_id.is_none() {
        crossing.note(
            "/PartnerProductID",
            "…and no product is named at all, so the provider has nothing to re-derive the price \
             from. A record with no `PartnerProductID` settles at whatever the two parties'  \
             framework agreement says, which is not a document",
        );
    }

    // ── The four timestamps, which is where this wire is richer ─────────────
    let connected = (charging_start - cdr.started_at).whole_seconds()
        + (cdr.ended_at - charging_end).whole_seconds();
    if connected > 0 {
        crossing.note(
            "/ChargingStart",
            format!(
                "{connected} s of this session are connected time rather than charging time, and \
                 OICP states it: `SessionStart`/`SessionEnd` bound the session and \
                 `ChargingStart`/`ChargingEnd` bound the energy. OCPI has no second pair and a \
                 reader there attributes the same span to whichever measured period precedes it"
            ),
        );
    }

    // ── What is inside the measured value ───────────────────────────────────
    //
    // The same account the OCPI crossing already gives, on the wire that had
    // not been given it. `CL` states how much of the register is cable or
    // rectification rather than vehicle `[OCMF Tab. 7, CL]`; nothing is
    // subtracted, because the compensation is already inside the value that
    // settles — and a partner disputing the energy asks exactly this.
    // `[REA 6-A §3.2]` makes telling the affected party what is inside a
    // measured value a duty rather than a courtesy, and a settlement partner is
    // one (D253).
    if let Some(loss) = cdr.evidence.as_ref().and_then(|e| e.compensated_loss) {
        crossing.note(
            "/ConsumedEnergy",
            format!(
                "{loss} of this energy is compensated cable or rectification loss and is \
                 already inside the value that settles `[OCMF Tab. 7, CL]`. OICP has no field \
                 for it, and it is the first thing a partner disputing the energy asks about \
                 `[REA 6-A §3.2]`"
            ),
        );
    }

    // ── The periods, which do not cross ─────────────────────────────────────
    if cdr.periods.len() > 1 {
        crossing.note(
            "/MeterValueInBetween",
            format!(
                "this record has {} charging periods and OICP's `MeterValueInBetween` is a list \
                 of kilowatt-hour readings with **no timestamps** \
                 `[OICP 2.3 §ChargeDetailRecord]`. The intervals a tiered or time-of-day tariff \
                 is rated over therefore do not cross, and a provider re-rating this session can \
                 only price it as one block",
                cdr.periods.len()
            ),
        );
    }
}

/// The two register readings, checked against the record they belong to.
///
/// OICP *defines* `ConsumedEnergy` as `MeterValueEnd − MeterValueStart` and
/// Hubject validates it, so supplying the readings turns that definition into a
/// check this side runs first — the difference between a note to an operator and
/// a record refused after the driver has gone.
fn register(
    cdr: &Cdr,
    context: &Context<'_>,
    crossing: &mut Crossing<()>,
) -> Result<(Option<Number>, Option<Number>), RoamError> {
    let Some(window) = context.meter else {
        crossing.note(
            "/MeterValueStart",
            "no register readings were supplied, so `ConsumedEnergy` stands alone. OICP *defines* \
             it as `MeterValueEnd - MeterValueStart`, so a provider that checks the definition \
             finds nothing to check it against",
        );
        return Ok((None, None));
    };
    let metered = window.end.kwh() - window.start.kwh();
    if metered != cdr.total_energy.kwh() {
        return Err(RoamError::RegisterDisagrees {
            metered: Energy::from_kwh(metered.max(Decimal::ZERO))
                .unwrap_or(Energy::ZERO)
                .to_string(),
            total: cdr.total_energy.to_string(),
        });
    }
    Ok((
        Some(Number::new(window.start.kwh())),
        Some(Number::new(window.end.kwh())),
    ))
}

/// The span energy actually flowed in, from the record's own periods.
///
/// # Not the periods' outer bounds
///
/// `ChargingStart` and `ChargingEnd` bound the **energy**, and a canonical
/// record's period list covers the whole session: the split holds the register
/// flat across the intervals the session says nothing flowed in, so the first
/// period of an ordinary 2.0.1 transaction is the wait before the charge begins
/// (D190). Taking the outer bounds would put `ChargingStart` at the moment the
/// cable went in and hand the provider five minutes of charging that no meter
/// reading supports — which is precisely the inference OICP's second pair of
/// timestamps exists to remove.
///
/// A record whose periods are all non-charging falls back to the session's own
/// window, and the caller has already been refused for an empty list, so
/// neither fallback invents a span.
fn metered_window(cdr: &Cdr) -> (time::OffsetDateTime, time::OffsetDateTime) {
    let mut charging = cdr
        .periods
        .iter()
        .filter(|period| period.activity.transfers_energy())
        .peekable();
    if charging.peek().is_none() {
        return (cdr.started_at, cdr.ended_at);
    }
    let (mut start, mut end) = (cdr.ended_at, cdr.started_at);
    for period in charging {
        start = start.min(period.start);
        end = end.max(period.end);
    }
    (start, end)
}

/// Who charged, in the one shape OICP has for it.
///
/// # `AutoCharge` has no home here, and inventing one would be a false statement
///
/// OCPI collapses Plug & Charge and `AutoCharge` into a single `AUTH_REQUEST`
/// that names neither, and the crossing there reports the lost distinction.
/// OICP's `PlugAndChargeIdentification` **names** ISO 15118, so writing a
/// MAC-address session into it asserts a contract certificate that was never
/// presented — which is a lie rather than a loss, and gets a refusal (D233).
fn identification(path: AuthPath, token: &RoamingToken) -> Result<Identification, RoamError> {
    use oicp_kit::types::{
        PlugAndChargeIdentification, QrCodeIdentification, RemoteIdentification,
        RfidMifareFamilyIdentification,
    };

    let contract = || {
        token
            .contract_id
            .as_str()
            .parse()
            .map_err(|error| RoamError::NotOnTheWire {
                field: "EvcoID",
                value: token.contract_id.as_str().to_owned(),
                detail: format!("{error}"),
            })
    };
    let uid = || {
        token.uid.parse().map_err(|error| RoamError::NotOnTheWire {
            field: "UID",
            value: token.uid.clone(),
            detail: format!("{error}"),
        })
    };

    Ok(match path {
        AuthPath::AutoCharge => return Err(RoamError::AutoChargeNotExpressible),
        AuthPath::PlugAndCharge => Identification::PlugAndCharge(
            PlugAndChargeIdentification::builder()
                .evco_id(contract()?)
                .build_unchecked(),
        ),
        AuthPath::RemoteCommand => Identification::Remote(
            RemoteIdentification::builder()
                .evco_id(contract()?)
                .build_unchecked(),
        ),
        // A card, whether the station decided locally or asked the provider.
        AuthPath::LocalList | AuthPath::Roaming => Identification::RfidMifareFamily(
            RfidMifareFamilyIdentification::builder()
                .uid(uid()?)
                .build_unchecked(),
        ),
        // No contract at all: OICP's QR-code member is the app and web flow.
        AuthPath::AdHoc => Identification::QrCode(
            QrCodeIdentification::builder()
                .evco_id(contract()?)
                .build_unchecked(),
        ),
        // `AuthPath` is `#[non_exhaustive]`, and a path a later release adds
        // must not become an RFID card by falling through. A wrong
        // identification bills the session to somebody else's contract, and
        // nothing downstream can tell that it was a default rather than a fact
        // — the same reason `disqualifies` is total over the upstream
        // vocabulary one crate down.
        other => {
            return Err(RoamError::NotOnTheWire {
                field: "Identification",
                value: format!("{other:?}"),
                detail: "OICP names five ways a driver can be identified and this build does not \
                         know which of them this authorisation path is"
                    .to_owned(),
            });
        }
    })
}

/// The signed records, narrowed onto OICP's three-valued metering status.
fn signed_values(
    payloads: &[SignedPayload],
    crossing: &mut Crossing<()>,
) -> Result<Option<Vec<SignedMeteringValue>>, RoamError> {
    if payloads.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(payloads.len());
    let mut dropped_plain = false;
    for payload in payloads {
        let length = payload.signed_data.len();
        if length > SIGNED_VALUE_LIMIT {
            return Err(RoamError::SignedValueTooLong {
                length,
                limit: SIGNED_VALUE_LIMIT,
            });
        }
        dropped_plain |= !payload.plain_data.is_empty();
        out.push(
            SignedMeteringValue::builder()
                .signed_metering_value(text::<3000>(&payload.signed_data).map_err(|value| {
                    RoamError::TooLong {
                        field: "SignedMeteringValue",
                        len: value.len(),
                        max: SIGNED_VALUE_LIMIT,
                    }
                })?)
                .maybe_metering_status(metering_status(&payload.nature))
                .build_unchecked(),
        );
    }
    if dropped_plain {
        crossing.note(
            "/SignedMeteringValues",
            "OICP has no field for the human-readable rendering beside a signed record — a \
             `SignedMeteringValue` is the signed string and its status. OCPI carries it as \
             `plain_data`, and it did not cross",
        );
    }
    Ok(Some(out))
}

/// The three values OICP has for where in a session a reading was taken.
///
/// `None` for anything else, which is the honest answer: an OCMF record's
/// nature carries the format's own pagination and OICP has three words.
fn metering_status(nature: &str) -> Option<MeteringStatus> {
    match nature.trim().to_ascii_lowercase().as_str() {
        "start" | "begin" => Some(MeteringStatus::Start),
        "end" | "stop" => Some(MeteringStatus::End),
        "progress" | "sample" | "intermediate" => Some(MeteringStatus::Progress),
        _ => None,
    }
}

/// What a driver needs to check the readings for themselves.
fn calibration_info(cdr: &Cdr, context: &Context<'_>) -> Option<CalibrationLawVerificationInfo> {
    let encoding = cdr
        .evidence
        .as_ref()
        .map(|evidence| evidence.encoding_method.clone());
    if encoding.is_none()
        && context.public_key.is_none()
        && context.calibration_certificate.is_none()
        && context.verification_url.is_none()
    {
        return None;
    }
    Some(
        CalibrationLawVerificationInfo::builder()
            .maybe_calibration_law_certificate_id(
                context
                    .calibration_certificate
                    .as_deref()
                    .and_then(|value| text::<100>(value).ok()),
            )
            .maybe_public_key(
                context
                    .public_key
                    .as_deref()
                    .and_then(|value| text::<1000>(value).ok()),
            )
            .maybe_metering_signature_url(
                context
                    .verification_url
                    .as_deref()
                    .and_then(|value| text::<200>(value).ok()),
            )
            .maybe_metering_signature_encoding_format(
                encoding.as_deref().and_then(|value| text::<50>(value).ok()),
            )
            .build_unchecked(),
    )
}

/// A bounded string, or the value that did not fit.
fn text<const N: usize>(value: &str) -> Result<Text<N>, String> {
    Text::<N>::new(value).map_err(|_| value.to_owned())
}

/// The energy the periods account for, as a check on the record that carries
/// them.
///
/// The OICP counterpart of [`crate::ocpi::cdr::check_conserves`]: a partner's
/// record is checked against its own arithmetic before anything is read out of
/// it.
///
/// # Errors
///
/// [`RoamError::RegisterDisagrees`] when the two register readings do not
/// produce the stated `ConsumedEnergy`, which is what OICP defines it to be.
pub fn check_conserves(record: &ChargeDetailRecord) -> Result<Energy, RoamError> {
    let stated = record.consumed_energy.get();
    if let Some(metered) = record.metered_energy()
        && metered.get() != stated
    {
        return Err(RoamError::RegisterDisagrees {
            metered: metered.get().to_string(),
            total: stated.to_string(),
        });
    }
    Energy::from_kwh(stated).map_err(|_| RoamError::UnreadableField {
        field: "ConsumedEnergy".to_owned(),
        detail: format!("{stated} is not an energy this model can hold"),
    })
}

/// The signed records on an arriving document, for the registry to check.
///
/// The mirror of [`crate::ocpi::cdr::inbound_payloads`]: verification is
/// the receiver's, against the receiver's own registry, and the payloads come
/// back verbatim so the bytes the meter signed are the bytes that are checked.
#[must_use]
pub fn inbound_payloads(record: &ChargeDetailRecord) -> Vec<SignedPayload> {
    record
        .signed_metering_values
        .iter()
        .flatten()
        .filter_map(|value| {
            Some(SignedPayload {
                nature: value
                    .metering_status
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                signed_data: value.signed_metering_value.as_ref()?.as_str().to_owned(),
                // OICP never carried one.
                plain_data: String::new(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_is_placed_at_the_point_of_the_session_oicp_has_a_word_for() {
        assert_eq!(metering_status("Start"), Some(MeteringStatus::Start));
        assert_eq!(metering_status("begin"), Some(MeteringStatus::Start));
        assert_eq!(metering_status("End"), Some(MeteringStatus::End));
        assert_eq!(
            metering_status(" Progress "),
            Some(MeteringStatus::Progress)
        );

        // OCMF's own pagination — `T3`, `F1` — is not one of the three words,
        // and inventing a placement for it would tell a provider that a reading
        // was taken at the end of the session when nothing said so. `None` is
        // the honest answer and the field is optional for exactly this.
        assert_eq!(metering_status("T3"), None);
        assert_eq!(metering_status(""), None);
    }
}
