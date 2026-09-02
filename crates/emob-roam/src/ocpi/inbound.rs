//! A partner's CDR, read back into the canonical model.
//!
//! [`super::cdr::to_ocpi`] carries a record out; this reads one in. The two are
//! not mirror images, and the asymmetry is the whole content of this module.
//!
//! # Read the document, then convert it — never the other way round
//!
//! [`super::preflight::preflight`] asks OCPI's own questions of the document that
//! arrived: do the periods sum to the total, do the durations agree with the
//! timestamps, does the contract identifier survive its own check digit. Those
//! questions have to be asked **first**, because every conversion makes
//! decisions and every decision *repairs* something. A record whose periods run
//! backwards has an obvious canonical form and no honest one.
//!
//! So [`from_ocpi`] assumes nothing about the document's coherence and refuses
//! rather than repairs: what it cannot read, it names.
//!
//! # What OCPI does not carry, and what this does about it
//!
//! | Canonical | OCPI | What happens |
//! |---|---|---|
//! | a period's `end` | periods have a start only | taken from the next period's start, and the last from `end_date_time` — the reading the specification prescribes, and `ocpi-kit`'s `period_spans` implements |
//! | `charging` | a period's *dimensions* | `TIME` says charging, `PARKING_TIME` says not. A period stating neither states nothing, and is reported rather than guessed |
//! | `provenance` | nothing at all | [`Provenance::Interpolated`], because a number whose provenance nobody stated is not one this side may call measured |
//! | `auth_path` | three `auth_method` values for six paths | narrowed with the token type, and noted where it cannot be |
//! | `cost` | totals, with no unit prices | **not** reconstructed. See below |
//!
//! # The cost does not come back
//!
//! A canonical [`Cost`](emob_cdr::Cost) carries a [`Rated`](emob_tariff::Rated)
//! — one line per distinct price, each reproducing its own amount from its own
//! quantity and unit price. OCPI carries totals and no unit prices at all, so
//! rebuilding one would mean inventing the numbers that make it add up, and
//! `emob_cdr::validate` would then check the arithmetic of a document this
//! crate wrote rather than of the one that arrived.
//!
//! An eMSP re-rates anyway: what it owes its driver is its own retail tariff,
//! and what it owes the CPO is a comparison. So the canonical record comes back
//! **unpriced**, and [`Inbound::stated_total`] carries what the partner says it
//! costs, for exactly that comparison.

use emob_cdr::{Cdr, CdrKey, ChargingPeriod, EvidenceRef};
use emob_core::{Currency, Direction, Energy, Money, QuarterHour};
use emob_session::{AuthPath, Provenance};
use ocpi_kit::v2_3_0::cdrs::{AuthMethod, CdrDimensionType};
use ocpi_kit::v2_3_0::tokens::TokenType;
use rust_decimal::Decimal;

use crate::crossing::Crossing;
use crate::error::RoamError;

/// A partner's record in the canonical model, with what the partner says it
/// costs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    /// The record, **unpriced** — see the module documentation.
    pub cdr: Cdr,
    /// What the partner's own document says the session came to, gross.
    ///
    /// Kept beside the record rather than inside it, because it is the
    /// partner's claim rather than this side's computation. Re-rating produces
    /// the other number, and the pair is what a settlement conversation is
    /// about.
    pub stated_total: Money,
}

/// Read a partner's OCPI CDR into the canonical model.
///
/// `evidence` is supplied by the caller because producing it means **verifying**
/// the signed records against the receiver's own key registry — which is
/// `emob-eichrecht`'s job and needs a registry this crate has no business
/// holding. [`super::cdr::inbound_payloads`] hands over the payloads to verify;
/// `EvidenceRef::from_evidence` turns the result into the argument.
///
/// Passing `None` is admissible and means what it says: no signed records back
/// this claim. `emob_cdr::validate` grades that against the regime the billing
/// layer is in.
///
/// # Errors
///
/// [`RoamError::NoPeriods`] for a record with nothing behind its total;
/// [`RoamError::UnreadableField`] for an identifier, currency or quantity this
/// side's types refuse — which is a refusal rather than a repair, because every
/// repair is a number invented on behalf of somebody who will be invoiced for
/// it.
pub fn from_ocpi(
    cdr: &ocpi_kit::v2_3_0::Cdr,
    evidence: Option<EvidenceRef>,
) -> Result<Crossing<Inbound>, RoamError> {
    if cdr.charging_periods.is_empty() {
        return Err(RoamError::NoPeriods);
    }

    let mut crossing = Crossing::lossless(());

    let key = super::cdr::key_of(cdr).ok_or_else(|| RoamError::UnreadableField {
        field: "id".to_owned(),
        detail: format!(
            "{}*{}/{} is not a party and a record id this side can key a ledger on",
            cdr.country_code.as_str(),
            cdr.party_id.as_str(),
            cdr.id.as_str()
        ),
    })?;

    let periods = periods_of(cdr, &mut crossing)?;
    let total_energy =
        Energy::from_kwh(cdr.total_energy.get()).map_err(|error| RoamError::UnreadableField {
            field: "total_energy".to_owned(),
            detail: error.to_string(),
        })?;

    let token_type = cdr.cdr_token.token_type.clone();
    let auth_path = auth_path_of(cdr.auth_method, &token_type);
    if let Some(reason) = auth_path_note(cdr.auth_method, &token_type) {
        crossing.note("/auth_method", reason);
    }

    // Nothing states a provenance, so nothing may claim one — and a settlement
    // process that treats an interpolated slot as authoritative is the reason
    // the field exists. Said once here rather than once per period.
    crossing.note(
        "/charging_periods",
        "OCPI has no field for how a period's energy was arrived at, so every period comes back \
         interpolated. That is the weaker answer and the only honest one: a number whose \
         provenance nobody stated is not one this side may call measured",
    );

    let currency =
        Currency::new(cdr.currency.as_str()).map_err(|error| RoamError::UnreadableField {
            field: "currency".to_owned(),
            detail: error.to_string(),
        })?;

    let inbound = Inbound {
        cdr: Cdr {
            key,
            session_id: session_id_of(cdr, &key_hint(cdr))?,
            evse_id: emob_core::EvseId::parse(cdr.cdr_location.evse_id.as_str()).map_err(
                |error| RoamError::UnreadableField {
                    field: "cdr_location/evse_id".to_owned(),
                    detail: error.to_string(),
                },
            )?,
            started_at: cdr.start_date_time.into(),
            ended_at: cdr.end_date_time.into(),
            auth_path,
            periods,
            total_energy,
            // OCPI's `ENERGY_EXPORT` is Session-only and `total_energy` carries
            // no sign, so every CDR that arrives is a draw. `preflight` blocks
            // a record that reports an export volume anyway, rather than this
            // silently reading it as one.
            direction: Direction::Import,
            evidence,
            cost: None,
            supersedes: cdr
                .credit_reference_id
                .as_ref()
                .and_then(|previous| previous.as_str().parse().ok())
                .map(|id| CdrKey {
                    party: key_party(cdr),
                    id,
                }),
        },
        stated_total: Money::new(cdr.total_cost.after_taxes().get(), currency),
    };

    Ok(crossing.map(|()| inbound))
}

/// The party a superseded record belongs to — the same CPO, by construction:
/// OCPI keys a CDR per `country_code`/`party_id` and a credit reference names a
/// record of the sender's own.
fn key_party(cdr: &ocpi_kit::v2_3_0::Cdr) -> emob_core::PartyId {
    emob_core::PartyId::new(cdr.country_code.as_str(), cdr.party_id.as_str())
        .unwrap_or_else(|_| emob_core::PartyId::new("XX", "XXX").expect("a literal party"))
}

/// A description of the record, for an error that cannot name a field.
fn key_hint(cdr: &ocpi_kit::v2_3_0::Cdr) -> String {
    format!(
        "{}*{}/{}",
        cdr.country_code.as_str(),
        cdr.party_id.as_str(),
        cdr.id.as_str()
    )
}

fn session_id_of(
    cdr: &ocpi_kit::v2_3_0::Cdr,
    hint: &str,
) -> Result<emob_core::SessionId, RoamError> {
    cdr.session_id
        .as_ref()
        .map_or_else(
            // OCPI makes `session_id` optional — *"can be omitted when the CPO
            // has no Session object"* — and the canonical model requires one,
            // because a record that cannot name its session cannot be
            // reconciled against one. The record's own id is the only other
            // identifier that is unique within the sender, so it stands in.
            || cdr.id.as_str(),
            ocpi_kit::types::CiString::as_str,
        )
        .parse()
        .map_err(|error: emob_core::IdError| RoamError::UnreadableField {
            field: "session_id".to_owned(),
            detail: format!("{hint}: {error}"),
        })
}

/// The periods, with the ends OCPI does not carry and the charging flag it
/// states in its dimensions.
fn periods_of(
    cdr: &ocpi_kit::v2_3_0::Cdr,
    crossing: &mut Crossing<()>,
) -> Result<Vec<ChargingPeriod>, RoamError> {
    let mut periods = Vec::with_capacity(cdr.charging_periods.len());
    let mut silent = 0_usize;

    for (index, span) in cdr.period_spans().enumerate() {
        let kwh = span
            .volume(CdrDimensionType::Energy)
            .or_else(|| span.volume(CdrDimensionType::EnergyImport))
            .map_or(Decimal::ZERO, ocpi_kit::types::Number::get);
        let energy = Energy::from_kwh(kwh).map_err(|error| RoamError::UnreadableField {
            field: format!("charging_periods/{index}/dimensions"),
            detail: error.to_string(),
        })?;

        // `charging` is **read**, not inferred from `energy == 0`: a car at 100 %
        // state of charge draws a rounding error, and a period that genuinely
        // measured nothing while still charging is a taper. OCPI states the
        // answer in the dimensions — `TIME` is time charging, `PARKING_TIME` is
        // time not charging `[OCPI 2.3.0 §mod_cdrs_cdrdimensiontype_enum]`.
        let charging = match (
            span.volume(CdrDimensionType::Time),
            span.volume(CdrDimensionType::ParkingTime),
        ) {
            (Some(_), None) => true,
            (None, Some(_)) => false,
            // Both, or neither. Both is a period the sender says was two things
            // at once; neither is a period that says nothing. Energy is the
            // only remaining evidence and it is weak — see the comment above —
            // so the count is reported rather than each occurrence.
            _ => {
                silent += 1;
                !energy.is_zero()
            }
        };

        periods.push(ChargingPeriod {
            quarter_hour: QuarterHour::containing(span.start.into()),
            start: span.start.into(),
            end: span.end.into(),
            energy,
            charging,
            provenance: Provenance::Interpolated,
        });
    }

    if silent > 0 {
        crossing.note(
            "/charging_periods",
            format!(
                "{silent} of {} periods state neither a TIME nor a PARKING_TIME volume, so \
                 whether the vehicle was charging was taken from whether energy moved. That \
                 reads a taper — a full battery drawing a rounding error — as occupancy, and \
                 `[AFIR Art. 5(4)]` prices the two differently",
                cdr.charging_periods.len()
            ),
        );
    }

    Ok(periods)
}

/// Which authorisation path an `auth_method` names, narrowed by the token type.
///
/// OCPI has three values for six paths, so the mapping is one-to-many in the
/// direction that matters. The token type carries the one distinction that can
/// be recovered: `AD_HOC_USER` is *"a one-time-use Token ID generated by a
/// server or app"*, which is the ad-hoc session, and `EMAID` on an
/// `AUTH_REQUEST` is the vehicle presenting a contract.
///
/// What cannot be recovered is Plug & Charge from `AutoCharge` — both are
/// `AUTH_REQUEST` with a contract — so the weaker of the two is taken.
/// Under-reporting an authorisation is never a fault; over-reporting it bills
/// a contract that was never presented.
#[must_use]
pub fn auth_path_of(method: AuthMethod, token: &TokenType) -> AuthPath {
    match (method, token) {
        (AuthMethod::Command, _) => AuthPath::RemoteCommand,
        // Nothing went out to a provider. A one-time token says why: there was
        // nobody to ask.
        (AuthMethod::Whitelist, TokenType::AdHocUser) => AuthPath::AdHoc,
        (AuthMethod::Whitelist, _) => AuthPath::LocalList,
        // A request went out, so a provider answered for a contract.
        // `AUTH_REQUEST` covers roaming, Plug & Charge and AutoCharge alike;
        // roaming is the one that claims least.
        (AuthMethod::AuthRequest, _) => AuthPath::Roaming,
    }
}

/// What the three values could not say about the six paths.
fn auth_path_note(method: AuthMethod, token: &TokenType) -> Option<String> {
    match (method, token) {
        (AuthMethod::AuthRequest, TokenType::Emaid) => Some(
            "`AUTH_REQUEST` with an EMAID token is Plug & Charge or AutoCharge — a contract \
             certificate the vehicle presented, or a MAC address off the wire — and OCPI has one \
             value for both [OCPI 2.3.0 §mod_cdrs_authmethod_enum]. This record comes back as an \
             ordinary roaming authorisation, which is the weaker claim; the signed meter data is \
             where the distinction survives, in the identification strength"
                .to_owned(),
        ),
        (AuthMethod::AuthRequest, _) => Some(
            "`AUTH_REQUEST` says a provider was asked and not which path asked it. This record \
             comes back as roaming, the claim that assumes least"
                .to_owned(),
        ),
        (AuthMethod::Whitelist, TokenType::AdHocUser) | (AuthMethod::Command, _) => None,
        (AuthMethod::Whitelist, _) => Some(
            "`WHITELIST` says the CPO decided without asking anybody, which is both a local \
             authorisation list and an ad-hoc session. The token type is not `AD_HOC_USER`, so \
             this comes back as a local list"
                .to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_type_recovers_the_one_distinction_auth_method_lost() {
        // `WHITELIST` says the CPO decided without asking anybody, which is
        // both a local list and an ad-hoc session. A one-time token says which.
        assert_eq!(
            auth_path_of(AuthMethod::Whitelist, &TokenType::AdHocUser),
            AuthPath::AdHoc
        );
        assert_eq!(
            auth_path_of(AuthMethod::Whitelist, &TokenType::Rfid),
            AuthPath::LocalList
        );
        assert_eq!(
            auth_path_of(AuthMethod::Command, &TokenType::AppUser),
            AuthPath::RemoteCommand
        );

        // What cannot be recovered: Plug & Charge and AutoCharge are one value.
        // The weaker claim is the one taken, and the note says why — a contract
        // that was never presented is not one this side may bill.
        assert_eq!(
            auth_path_of(AuthMethod::AuthRequest, &TokenType::Emaid),
            AuthPath::Roaming
        );
        let note = auth_path_note(AuthMethod::AuthRequest, &TokenType::Emaid).expect("a note");
        assert!(note.contains("Plug & Charge or AutoCharge"), "{note}");
        assert!(note.contains("weaker claim"), "{note}");

        // The two that are exact say nothing.
        assert!(auth_path_note(AuthMethod::Command, &TokenType::AppUser).is_none());
        assert!(auth_path_note(AuthMethod::Whitelist, &TokenType::AdHocUser).is_none());
    }
}
