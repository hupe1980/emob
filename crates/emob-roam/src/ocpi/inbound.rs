//! The pre-flight an eMSP owes itself before paying a partner's record.
//!
//! # Why this is not `emob_cdr::validate`
//!
//! That function checks a **canonical** CDR: one this workspace's types have
//! already accepted, whose energy is an [`emob_core::Energy`] and whose periods have both
//! ends. The record that arrives from a partner is none of those things yet.
//! It is JSON that decoded, and the questions worth asking of it are OCPI's
//! own — whether the periods add up to the total, whether the durations agree
//! with the timestamps, whether the parts of the cost add to the whole.
//!
//! Converting first and validating afterwards inverts the order. A conversion
//! has to make decisions — what a period's end is, what a missing dimension
//! means — and every one of those decisions *repairs* something, which is
//! exactly what a pre-flight is for finding. So the questions are asked of the
//! document that arrived.
//!
//! # Nothing here verifies a signature
//!
//! [`payloads`](super::cdr::inbound_payloads) hands back the signed records and
//! [`claimed_key`](super::cdr::claimed_key) hands back the key the document
//! carries. Verifying is `emob-eichrecht`'s job, against the key **this side's**
//! registry holds — never the one in the file, which is the artefact under
//! examination. A reader that verified against the document's own key would
//! prove only that whoever wrote it owned a private key.
//!
//! What the key in the document *is* good for is the comparison: one that
//! differs from the registered key is a dispute with an answer, and one that
//! matches narrows the argument to the numbers. That is the same reading the
//! transparency file's reader takes, one layer down.

use emob_core::Emaid;
use ocpi_kit::types::{Number, Validate};
use ocpi_kit::v2_3_0::cdrs::CdrDimensionType;
use rust_decimal::Decimal;

use super::cdr::exact_hours;

/// How bad a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth knowing; settlement may proceed.
    Warning,
    /// Settlement must not proceed on this record.
    Blocking,
}

/// Something wrong with a record that arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Finding {
    /// The charging periods do not sum to `total_energy`.
    ///
    /// The first question to ask of a record somebody else built, and the one
    /// a canonical [`Cdr`](emob_cdr::Cdr) answers by construction. A partner
    /// whose periods do not sum to their own total is a partner whose
    /// re-rating will not match theirs.
    DoesNotConserve {
        /// What the periods add up to.
        periods: Decimal,
        /// What the record claims.
        total: Decimal,
    },
    /// `charging_periods` is empty, which OCPI's cardinality `+` forbids.
    NoPeriods,
    /// The record ends before it starts.
    EndsBeforeItStarts,
    /// A period starts outside the window the record states.
    PeriodOutsideWindow {
        /// Which period, by index.
        index: usize,
    },
    /// The periods are not in time order.
    ///
    /// OCPI has no `end` on a period, so a reader derives every period's span
    /// from the **next** one's start. Out of order, those spans are negative
    /// and every duration computed from them is wrong — silently, because
    /// nothing in the document is individually invalid.
    PeriodsOutOfOrder {
        /// The first index that goes backwards.
        index: usize,
    },
    /// `total_time` disagrees with the record's own timestamps.
    ///
    /// Both are in the document and they are supposed to be the same fact.
    /// A duration in hours often has to be rounded — see [`preflight`] for the
    /// tolerance and why it is a choice rather than a derivation — but past
    /// that, one of the two is wrong and the receiver cannot say which.
    DurationDisagrees {
        /// What `total_time` says, in hours.
        stated: Decimal,
        /// What `end_date_time - start_date_time` says, in hours.
        implied: Decimal,
    },
    /// More of the session was spent not charging than the session lasted.
    ParkingExceedsTotal {
        /// `total_parking_time`.
        parking: Decimal,
        /// `total_time`.
        total: Decimal,
    },
    /// The per-dimension costs do not add up to `total_cost`.
    ///
    /// A warning rather than a block: each part is rounded to the currency's
    /// minor unit and the whole is rounded once, so with several dimensions
    /// they can legitimately differ by a minor unit. It is worth knowing
    /// because it is also what a made-up breakdown looks like.
    CostsDoNotAddUp {
        /// What the parts come to.
        parts: Decimal,
        /// What `total_cost` says.
        total: Decimal,
    },
    /// The contract identifier fails its own check digit.
    ///
    /// The inbound half of the check the outbound crossing makes. An id that
    /// lost a character in transcription still parses and still routes — to a
    /// contract this provider does not hold, which is a payment about to be
    /// applied to the wrong driver.
    ContractCheckDigit {
        /// The identifier as it arrived.
        id: String,
    },
    /// The record carries no signed metering data.
    ///
    /// Blocking for a German session and not for every session, which is why
    /// [`preflight`] takes the question as an argument: under `[MessEG §33]` a
    /// measured value may be billed only where the affected party can check
    /// it, and the eMSP is the party that has to answer the driver.
    NoSignedData,
    /// A signed value arrived with nothing in it.
    EmptySignedValue {
        /// Which one, by index.
        index: usize,
    },
    /// `total_energy` is negative.
    ///
    /// OCPI has no direction on a CDR's total, so a negative one is either a
    /// V2G discharge nobody agreed a sign convention for, or a fault. Either
    /// way it is not a draw this provider should pay for.
    NegativeEnergy {
        /// The value that arrived.
        energy: Decimal,
    },
    /// A period reports energy fed back to the grid.
    ///
    /// `ENERGY_EXPORT` is "Session Only" and [`Self::SchemaViolation`] reports
    /// its presence. This is the half that moves money: `total_energy` is "Total
    /// energy charged", so an export volume has nowhere in the record to go, and
    /// neither answer is safe. Adding it nets a discharge against a draw —
    /// what [`Direction`](emob_core::Direction) exists to prevent, leaving both
    /// balance groups wrong `[A6 §IV.1]` — and dropping it settles a record
    /// while ignoring energy the record reports.
    ///
    /// So it blocks, as the outbound crossing does with the same session going
    /// the other way (`RoamError::ExportNotExpressible`).
    ExportEnergyInCdr {
        /// Which period, by index.
        index: usize,
        /// How much it reports.
        energy: Decimal,
    },
    /// A non-credit record whose id is longer than the specification allows.
    ///
    /// *"Normal (non-credit) CDRs SHALL only have an ID with a maximum length
    /// of 36"* — the extra three characters are room to append to a credit.
    IdTooLongForNonCredit {
        /// How long it is.
        len: usize,
    },
    /// The record does not satisfy OCPI's own schema rules.
    ///
    /// Reported rather than fatal, and separately from everything above,
    /// because `ocpi-kit` decodes permissively on purpose: a peer that
    /// overruns a `string(45)` must not make a whole page of CDRs
    /// undecodable. The violation still has to reach somebody.
    SchemaViolation {
        /// RFC 6901 pointer to the value.
        pointer: String,
        /// What is wrong with it.
        message: String,
    },
}

impl Finding {
    /// Whether this finding blocks settlement.
    #[must_use]
    pub const fn severity(&self) -> Severity {
        match self {
            Self::DoesNotConserve { .. }
            | Self::NoPeriods
            | Self::EndsBeforeItStarts
            | Self::PeriodsOutOfOrder { .. }
            | Self::PeriodOutsideWindow { .. }
            | Self::ContractCheckDigit { .. }
            | Self::NoSignedData
            | Self::NegativeEnergy { .. }
            | Self::ExportEnergyInCdr { .. } => Severity::Blocking,
            Self::DurationDisagrees { .. }
            | Self::ParkingExceedsTotal { .. }
            | Self::CostsDoNotAddUp { .. }
            | Self::EmptySignedValue { .. }
            | Self::IdTooLongForNonCredit { .. }
            | Self::SchemaViolation { .. } => Severity::Warning,
        }
    }
}

impl core::fmt::Display for Finding {
    #[allow(clippy::too_many_lines)]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DoesNotConserve { periods, total } => write!(
                f,
                "the charging periods sum to {periods} kWh and `total_energy` says {total}: one \
                 of the two is wrong and this side cannot say which"
            ),
            Self::NoPeriods => f.write_str(
                "the record has no charging periods, which OCPI's cardinality `+` forbids — a \
                 total with nothing behind it cannot be re-rated",
            ),
            Self::EndsBeforeItStarts => f.write_str("`end_date_time` is before `start_date_time`"),
            Self::PeriodOutsideWindow { index } => write!(
                f,
                "charging period {index} starts outside the window the record itself states"
            ),
            Self::PeriodsOutOfOrder { index } => write!(
                f,
                "charging period {index} starts before the one before it. An OCPI period has no \
                 end, so every span is derived from the next period's start — out of order, \
                 every duration in this record is wrong and nothing in it is individually invalid"
            ),
            Self::DurationDisagrees { stated, implied } => write!(
                f,
                "`total_time` says {stated} h and the record's own timestamps say {implied} h. \
                 Both are in the document and they are the same fact"
            ),
            Self::ParkingExceedsTotal { parking, total } => write!(
                f,
                "`total_parking_time` is {parking} h and `total_time` is {total} h: more of the \
                 session was spent not charging than the session lasted"
            ),
            Self::CostsDoNotAddUp { parts, total } => write!(
                f,
                "the per-dimension costs come to {parts} and `total_cost` says {total}. A minor \
                 unit of difference is rounding; more than that is a breakdown that was not \
                 computed from this total"
            ),
            Self::ContractCheckDigit { id } => write!(
                f,
                "the contract id `{id}` fails its own check digit, and it is what decides which \
                 driver this is billed to"
            ),
            Self::NoSignedData => f.write_str(
                "the record carries no signed metering data, so nothing here can be put in front \
                 of a driver who disputes it [MessEG §33]",
            ),
            Self::EmptySignedValue { index } => {
                write!(f, "signed value {index} carries no data")
            }
            Self::NegativeEnergy { energy } => write!(
                f,
                "`total_energy` is {energy}, and OCPI puts no sign convention on it — this is \
                 either a discharge nobody agreed terms for or a fault, and neither is a draw to \
                 pay for"
            ),
            Self::ExportEnergyInCdr { index, energy } => write!(
                f,
                "period {index} reports {energy} kWh fed back to the grid, which `total_energy` \
                 cannot hold: adding it would net a discharge against a draw and dropping it \
                 would settle a record while ignoring energy the record reports"
            ),
            Self::IdTooLongForNonCredit { len } => write!(
                f,
                "this is not a credit CDR and its id is {len} characters: the specification \
                 allows 36, the extra three being room to append to a credit"
            ),
            Self::SchemaViolation { pointer, message } => write!(f, "{pointer}: {message}"),
        }
    }
}

/// Everything wrong with a record that arrived.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Every finding, in the order they were checked.
    pub findings: Vec<Finding>,
}

impl Report {
    /// The findings that block settlement.
    pub fn blocking(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.severity() == Severity::Blocking)
    }

    /// The findings that are merely worth knowing.
    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.severity() == Severity::Warning)
    }

    /// Whether this record may be paid.
    #[must_use]
    pub fn is_settleable(&self) -> bool {
        self.blocking().next().is_none()
    }

    /// One line per finding, for an operator queue.
    pub fn reasons(&self) -> impl Iterator<Item = String> + '_ {
        self.findings.iter().map(ToString::to_string)
    }
}

/// Whether this record has to carry signed metering data to be payable.
///
/// An argument rather than a constant, because the same node peers with
/// parties in jurisdictions that do not ask. Under `[MessEG §33]` a German
/// session's measured value may be billed only where the affected party can
/// check it, and a record without the signed data is one the eMSP cannot put
/// in front of a driver who disputes it — but a provider settling elsewhere is
/// not in breach of anything by accepting one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignedDataPolicy {
    /// A record without signed metering data is not payable.
    Required,
    /// Its absence is not this check's business.
    #[default]
    Optional,
}

/// Every problem with a partner's CDR at once, separated into what blocks
/// settlement and what is worth knowing.
///
/// Nothing is repaired. A pre-flight that quietly fixed what it found would
/// make the record payable and leave the disagreement in place — which is the
/// failure it exists to surface, wearing its own uniform.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn preflight(cdr: &ocpi_kit::v2_3_0::Cdr, signed_data: SignedDataPolicy) -> Report {
    let mut findings = Vec::new();

    // ── The arithmetic ──────────────────────────────────────────────────
    if cdr.charging_periods.is_empty() {
        findings.push(Finding::NoPeriods);
    }

    // Both spellings of *energy drawn*. `ENERGY` is the only one OCPI permits
    // in a CDR and `SchemaViolation` reports `ENERGY_IMPORT` where it appears —
    // but that report is a warning, and a warning must not carry a quantity out
    // of the check that exists to find it: reading one spelling would hide a
    // partner's own kilowatt-hours from their own conservation sum.
    let summed: Decimal = cdr
        .charging_periods
        .iter()
        .filter_map(|period| {
            period
                .volume(CdrDimensionType::Energy)
                .or_else(|| period.volume(CdrDimensionType::EnergyImport))
        })
        .map(Number::get)
        .sum();
    if summed != cdr.total_energy.get() {
        findings.push(Finding::DoesNotConserve {
            periods: summed,
            total: cdr.total_energy.get(),
        });
    }

    // Energy in the other direction, which has nowhere in this record to go.
    // Never added to the sum above: `total_energy` is "Total energy charged",
    // and netting a discharge against a draw is the error `Direction` exists to
    // prevent `[A6 §IV.1]`.
    for (index, period) in cdr.charging_periods.iter().enumerate() {
        if let Some(exported) = period.volume(CdrDimensionType::EnergyExport)
            && !exported.get().is_zero()
        {
            findings.push(Finding::ExportEnergyInCdr {
                index,
                energy: exported.get(),
            });
        }
    }

    if cdr.total_energy.get().is_sign_negative() && !cdr.total_energy.get().is_zero() {
        findings.push(Finding::NegativeEnergy {
            energy: cdr.total_energy.get(),
        });
    }

    // ── The window, and the order inside it ─────────────────────────────
    if cdr.end_date_time < cdr.start_date_time {
        findings.push(Finding::EndsBeforeItStarts);
    }

    let mut previous: Option<ocpi_kit::types::DateTime> = None;
    for (index, period) in cdr.charging_periods.iter().enumerate() {
        if period.start_date_time < cdr.start_date_time
            || period.start_date_time > cdr.end_date_time
        {
            findings.push(Finding::PeriodOutsideWindow { index });
        }
        if previous.is_some_and(|earlier| period.start_date_time < earlier) {
            findings.push(Finding::PeriodsOutOfOrder { index });
        }
        previous = Some(period.start_date_time);
    }

    // ── The durations, which are in the document twice ──────────────────
    if cdr.end_date_time >= cdr.start_date_time {
        let seconds = cdr.end_date_time.unix_timestamp() - cdr.start_date_time.unix_timestamp();
        let implied = Decimal::from(seconds) / Decimal::from(3600);
        let stated = cdr.total_time.get();

        // An exact duration has to match exactly: there was nothing to round,
        // so a difference is a disagreement about the facts.
        //
        // One that is *not* exact was rounded by whoever sent it, and OCPI
        // gives the field no scale — so there is no "largest rounding the
        // field can hide" to use as a tolerance. Half a second is the line
        // drawn here, and it is a choice rather than a derivation: it admits
        // the four decimal places OCPI's own examples carry (which can be
        // wrong by at most 0.18 s) and rejects a partner rounding to two
        // (wrong by up to 18 s, which at an occupancy fee is real money and
        // worth a warning rather than silence).
        let tolerance = if exact_hours(seconds).is_some() {
            Decimal::ZERO
        } else {
            Decimal::ONE / Decimal::from(2 * 3600)
        };
        if (stated - implied).abs() > tolerance {
            findings.push(Finding::DurationDisagrees {
                stated,
                implied: implied.round_dp(6).normalize(),
            });
        }
    }

    if let Some(parking) = cdr.total_parking_time
        && parking.get() > cdr.total_time.get()
    {
        findings.push(Finding::ParkingExceedsTotal {
            parking: parking.get(),
            total: cdr.total_time.get(),
        });
    }

    // ── The money ───────────────────────────────────────────────────────
    let parts: Decimal = [
        cdr.total_energy_cost.as_ref(),
        cdr.total_time_cost.as_ref(),
        cdr.total_parking_cost.as_ref(),
        cdr.total_fixed_cost.as_ref(),
        cdr.total_reservation_cost.as_ref(),
    ]
    .into_iter()
    .flatten()
    .map(|price| price.after_taxes().get())
    .sum();
    let total = cdr.total_cost.after_taxes().get();
    // Only where a breakdown was actually sent: a record carrying `total_cost`
    // alone is uninformative rather than inconsistent.
    if !parts.is_zero() && parts != total {
        findings.push(Finding::CostsDoNotAddUp { parts, total });
    }

    // ── The identifier that decides which driver pays ───────────────────
    let contract = cdr.cdr_token.contract_id.as_str();
    if let Err(emob_core::IdError::BadCheckDigit { .. }) = Emaid::parse(contract) {
        findings.push(Finding::ContractCheckDigit {
            id: contract.to_owned(),
        });
    }

    // ── The evidence ────────────────────────────────────────────────────
    match &cdr.signed_data {
        None if signed_data == SignedDataPolicy::Required => findings.push(Finding::NoSignedData),
        Some(data) => {
            for (index, value) in data.signed_values.iter().enumerate() {
                if value.signed_data.as_str().trim().is_empty() {
                    findings.push(Finding::EmptySignedValue { index });
                }
            }
            if data.signed_values.is_empty() && signed_data == SignedDataPolicy::Required {
                findings.push(Finding::NoSignedData);
            }
        }
        None => {}
    }

    // ── The shape ───────────────────────────────────────────────────────
    if !cdr.is_credit()
        && cdr.id.as_str().chars().count() > ocpi_kit::v2_3_0::cdrs::NON_CREDIT_ID_MAX_LEN
    {
        findings.push(Finding::IdTooLongForNonCredit {
            len: cdr.id.as_str().chars().count(),
        });
    }

    if let Err(violations) = cdr.validate() {
        for violation in &violations {
            findings.push(Finding::SchemaViolation {
                pointer: violation.pointer.clone(),
                message: violation.message.clone(),
            });
        }
    }

    Report { findings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocpi_kit::types::{CiString, DateTime, OcpiString};
    use ocpi_kit::v2_3_0::Price;
    use ocpi_kit::v2_3_0::cdrs::{AuthMethod, CdrDimension, CdrLocation, CdrToken, ChargingPeriod};
    use ocpi_kit::v2_3_0::locations::{ConnectorFormat, ConnectorType, GeoLocation, PowerType};
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap()
    }

    fn at(text: &str) -> DateTime {
        text.parse().unwrap()
    }

    fn location() -> CdrLocation {
        CdrLocation::builder()
            .id(CiString::<36>::new("loc-1").unwrap())
            .address(OcpiString::<45>::new("Hauptstraße 12").unwrap())
            .city(OcpiString::<45>::new("Berlin").unwrap())
            .country(OcpiString::<3>::new("DEU").unwrap())
            .coordinates(GeoLocation::new("52.520008", "13.404954").unwrap())
            .evse_uid(CiString::<36>::new("evse-1").unwrap())
            .evse_id(CiString::<48>::new("DE*AB7*E840*6487").unwrap())
            .connector_id(CiString::<36>::new("1").unwrap())
            .connector_standard(ConnectorType::Iec62196T2Combo)
            .connector_format(ConnectorFormat::Socket)
            .connector_power_type(PowerType::Dc)
            .build()
    }

    fn token(contract: &str) -> CdrToken {
        CdrToken::builder()
            .country_code(CiString::<2>::new("NL").unwrap())
            .party_id(CiString::<3>::new("TNM").unwrap())
            .uid(CiString::<36>::new("045F2C").unwrap())
            .token_type(ocpi_kit::v2_3_0::tokens::TokenType::Rfid)
            .contract_id(CiString::<36>::new(contract).unwrap())
            .build()
    }

    fn period(start: &str, kwh: &str, hours: &str) -> ChargingPeriod {
        ChargingPeriod::builder()
            .start_date_time(at(start))
            .dimensions(vec![
                CdrDimension::new(CdrDimensionType::Energy, Number::new(dec(kwh))),
                CdrDimension::new(CdrDimensionType::Time, Number::new(dec(hours))),
            ])
            .build()
    }

    /// A well-formed half-hour, 18.000 kWh, €8.82 gross.
    fn arriving() -> ocpi_kit::v2_3_0::Cdr {
        ocpi_kit::v2_3_0::Cdr::builder()
            .country_code(CiString::<2>::new("DE").unwrap())
            .party_id(CiString::<3>::new("ABC").unwrap())
            .id(CiString::<39>::new("cdr-1").unwrap())
            .start_date_time(at("2026-01-02T10:00:00Z"))
            .end_date_time(at("2026-01-02T10:30:00Z"))
            .cdr_token(token("NL-TNM-C00122045-K"))
            .auth_method(AuthMethod::AuthRequest)
            .cdr_location(location())
            .currency(OcpiString::<3>::new("EUR").unwrap())
            .charging_periods(vec![
                period("2026-01-02T10:00:00Z", "10.000", "0.25"),
                period("2026-01-02T10:15:00Z", "8.000", "0.25"),
            ])
            .total_cost(Price::new(Number::new(dec("8.82"))))
            .total_energy(Number::new(dec("18.000")))
            .total_time(Number::new(dec("0.5")))
            .last_updated(at("2026-01-02T10:31:00Z"))
            .build()
    }

    /// The same periods, with their energy in a dimension OCPI forbids here.
    fn with_energy_dimension(kind: CdrDimensionType) -> ocpi_kit::v2_3_0::Cdr {
        let mut cdr = arriving();
        for (period, kwh) in cdr.charging_periods.iter_mut().zip(["10.000", "8.000"]) {
            period.dimensions = vec![CdrDimension::new(kind, Number::new(dec(kwh)))];
        }
        cdr
    }

    #[test]
    fn energy_in_the_import_dimension_still_reaches_the_conservation_check() {
        // `ENERGY_IMPORT` is "Session Only" and may not appear in a CDR, which
        // the schema check reports — as a *warning*, because a permissive
        // decoder must not make a page of CDRs unpayable over a spec nit.
        //
        // A warning that hides a quantity is a different thing. Summing only
        // `ENERGY` made these eighteen kilowatt-hours invisible to the one check
        // whose job is to find them, so a partner could state `total_energy: 0`
        // beside them and settle clean.
        let mut lying = with_energy_dimension(CdrDimensionType::EnergyImport);
        lying.total_energy = Number::new(dec("0"));

        let report = preflight(&lying, SignedDataPolicy::Optional);
        assert!(
            !report.is_settleable(),
            "a record claiming 0 kWh beside 18 kWh of periods was payable: {:?}",
            report.reasons().collect::<Vec<_>>()
        );
        assert!(matches!(
            report.blocking().next().unwrap(),
            Finding::DoesNotConserve { .. }
        ));

        // …and one whose arithmetic is coherent settles, with the deviation
        // recorded rather than swallowed. The spelling is wrong; the money is
        // not, and a note a partner can act on beats a refusal they cannot.
        let coherent = with_energy_dimension(CdrDimensionType::EnergyImport);
        let report = preflight(&coherent, SignedDataPolicy::Optional);
        assert!(report.is_settleable());
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::SchemaViolation { .. })),
            "the deviation has to reach somebody"
        );
    }

    #[test]
    fn energy_fed_back_to_the_grid_blocks_rather_than_being_netted_or_dropped() {
        // The mirror of `RoamError::ExportNotExpressible` on the way out. OCPI's
        // `total_energy` is "Total energy charged", so an export volume has
        // nowhere to go: netting it against the draw is what `Direction` exists
        // to prevent `[A6 §IV.1]`, and dropping it settles a record while
        // ignoring energy the record itself reports.
        let mut discharging = arriving();
        discharging.charging_periods[0]
            .dimensions
            .push(CdrDimension::new(
                CdrDimensionType::EnergyExport,
                Number::new(dec("4.000")),
            ));

        let report = preflight(&discharging, SignedDataPolicy::Optional);
        assert!(!report.is_settleable());
        assert!(
            report
                .blocking()
                .any(|f| matches!(f, Finding::ExportEnergyInCdr { index: 0, .. })),
            "{:?}",
            report.reasons().collect::<Vec<_>>()
        );

        // The draw itself is untouched: the export is refused, never subtracted.
        assert!(
            !report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::DoesNotConserve { .. }))
        );
    }

    #[test]
    fn a_well_formed_record_is_payable() {
        let report = preflight(&arriving(), SignedDataPolicy::Optional);
        assert!(
            report.is_settleable(),
            "{:?}",
            report.reasons().collect::<Vec<_>>()
        );
        assert_eq!(report.findings.len(), 0);
    }

    #[test]
    fn periods_that_do_not_add_up_block_payment() {
        let mut wrong = arriving();
        wrong.total_energy = Number::new(dec("20.000"));

        let report = preflight(&wrong, SignedDataPolicy::Optional);
        assert!(!report.is_settleable());
        assert!(matches!(
            report.blocking().next().unwrap(),
            Finding::DoesNotConserve { .. }
        ));
    }

    #[test]
    fn periods_out_of_order_are_caught_although_nothing_in_them_is_invalid() {
        // An OCPI period has no end, so a reader derives every span from the
        // next period's start. Out of order, every duration in the record is
        // wrong — and no individual member of the document is malformed.
        let mut shuffled = arriving();
        shuffled.charging_periods.swap(0, 1);

        let report = preflight(&shuffled, SignedDataPolicy::Optional);
        assert!(
            report
                .blocking()
                .any(|f| matches!(f, Finding::PeriodsOutOfOrder { index: 1 })),
            "{:?}",
            report.reasons().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_duration_that_contradicts_the_timestamps_is_reported() {
        // Both facts are in the document, and they are the same fact.
        let mut wrong = arriving();
        wrong.total_time = Number::new(dec("2.0"));

        let report = preflight(&wrong, SignedDataPolicy::Optional);
        assert!(
            report
                .warnings()
                .any(|f| matches!(f, Finding::DurationDisagrees { .. })),
            "{:?}",
            report.reasons().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_partner_rounding_a_duration_too_coarsely_is_worth_a_warning() {
        // Two decimal places on a duration in hours loses up to 18 seconds,
        // which at the occupancy fee [AFIR Art. 5(4)] permits is real money.
        // 22 minutes is 0.36666… h; a partner sending `0.37` is 12 s out.
        let mut coarse = arriving();
        coarse.end_date_time = at("2026-01-02T10:22:00Z");
        coarse.total_time = Number::new(dec("0.37"));
        coarse.charging_periods = vec![period("2026-01-02T10:00:00Z", "18.000", "0.37")];

        let report = preflight(&coarse, SignedDataPolicy::Optional);
        assert!(
            report
                .warnings()
                .any(|f| matches!(f, Finding::DurationDisagrees { .. })),
            "{:?}",
            report.reasons().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_duration_rounded_because_hours_cannot_hold_it_is_not_a_disagreement() {
        // Twenty minutes is a third of an hour. A partner who sent `0.3333`
        // did the only thing the field allows, and calling that a
        // contradiction would flag every session that does not land on a
        // multiple of nine seconds — which is most of them.
        let mut twenty = arriving();
        twenty.end_date_time = at("2026-01-02T10:20:00Z");
        twenty.total_time = Number::new(dec("0.3333"));
        twenty.charging_periods = vec![period("2026-01-02T10:00:00Z", "18.000", "0.3333")];

        let report = preflight(&twenty, SignedDataPolicy::Optional);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::DurationDisagrees { .. })),
            "{:?}",
            report.reasons().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_transcribed_contract_id_does_not_get_paid() {
        // The inbound half of the check the outbound crossing makes. This
        // payment is about to be applied to a contract nobody holds.
        let mut wrong = arriving();
        wrong.cdr_token = token("NL-TNM-C00122045-X");

        let report = preflight(&wrong, SignedDataPolicy::Optional);
        assert!(!report.is_settleable());
        assert!(
            report
                .blocking()
                .any(|f| matches!(f, Finding::ContractCheckDigit { .. }))
        );
    }

    #[test]
    fn a_provider_scheme_in_no_grammar_is_not_a_failed_check_digit() {
        let mut own = arriving();
        own.cdr_token = token("acct-9931-2026-fleet");

        let report = preflight(&own, SignedDataPolicy::Optional);
        assert!(
            report.is_settleable(),
            "an eMSP is free to use its own scheme: {:?}",
            report.reasons().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_german_session_without_signed_data_is_not_payable() {
        let report = preflight(&arriving(), SignedDataPolicy::Required);
        assert!(!report.is_settleable());
        assert_eq!(report.blocking().next().unwrap(), &Finding::NoSignedData);

        // …and elsewhere it is simply not this check's business.
        assert!(preflight(&arriving(), SignedDataPolicy::Optional).is_settleable());
    }

    #[test]
    fn a_breakdown_that_was_not_computed_from_the_total_is_worth_knowing() {
        let mut wrong = arriving();
        wrong.total_energy_cost = Some(Price::new(Number::new(dec("5.00"))));

        let report = preflight(&wrong, SignedDataPolicy::Optional);
        assert!(
            report
                .warnings()
                .any(|f| matches!(f, Finding::CostsDoNotAddUp { .. })),
            "{:?}",
            report.reasons().collect::<Vec<_>>()
        );
        assert!(
            report.is_settleable(),
            "a minor unit of rounding is not a reason to refuse a payment"
        );
    }

    #[test]
    fn a_record_carrying_only_a_total_is_uninformative_rather_than_inconsistent() {
        let report = preflight(&arriving(), SignedDataPolicy::Optional);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::CostsDoNotAddUp { .. }))
        );
    }

    #[test]
    fn every_problem_is_reported_at_once_and_nothing_is_repaired() {
        // A pre-flight that stopped at the first fault makes fixing a
        // partner's export an N-round-trip conversation, and one that quietly
        // repaired what it found would make the record payable and leave the
        // disagreement in place.
        let mut broken = arriving();
        broken.total_energy = Number::new(dec("-1.000"));
        broken.cdr_token = token("NL-TNM-C00122045-X");
        broken.charging_periods.clear();

        let report = preflight(&broken, SignedDataPolicy::Required);
        assert!(!report.is_settleable());

        for expected in [
            Finding::NoPeriods,
            Finding::NegativeEnergy {
                energy: dec("-1.000"),
            },
            Finding::ContractCheckDigit {
                id: "NL-TNM-C00122045-X".to_owned(),
            },
            Finding::NoSignedData,
        ] {
            assert!(
                report.findings.contains(&expected),
                "{expected} was not reported: {:?}",
                report.reasons().collect::<Vec<_>>()
            );
        }

        // …and the record that arrived is untouched.
        assert_eq!(broken.total_energy.get(), dec("-1.000"));
        assert!(broken.charging_periods.is_empty());
    }

    #[test]
    fn ocpis_own_schema_rules_are_reported_rather_than_swallowed() {
        // `ocpi-kit` decodes permissively on purpose — a peer that overruns a
        // `string(45)` must not make a whole page undecodable — so the
        // violation has to reach somebody, and this is where.
        let mut empty = arriving();
        empty.charging_periods.clear();

        let report = preflight(&empty, SignedDataPolicy::Optional);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::SchemaViolation { pointer, .. }
                    if pointer == "/charging_periods")),
            "{:?}",
            report.reasons().collect::<Vec<_>>()
        );
    }
}
