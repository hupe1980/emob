//! The session chain: what a set of signed records proves *together*.
//!
//! # Why signatures are not enough
//!
//! Each OCMF record is signed independently. An attacker — or, far more often,
//! a buggy backend — who drops the middle records of a session leaves a
//! sequence in which every remaining signature still verifies. The specification
//! is explicit that this is the verifier's job:
//!
//! > The cohesion between several individual data records is ensured by
//! > continuous pagination. In addition to the signature, this must be verified
//! > by a check component. The first record must be marked as the start of a
//! > charging process, the last as the end of the charging process. In between,
//! > no data records may have been removed or added.
//! >
//! > `[OCMF §Signing and Verification Process]`
//!
//! [`validate`] is that check component. It answers one question — *may this
//! session be billed, and for how much* — and it answers it with a
//! [`ChainReport`] that lists every reason it might not be, rather than a bare
//! boolean.
//!
//! # What it enforces
//!
//! | Rule | Source |
//! |---|---|
//! | Pagination is contiguous and ascending within one context | `[OCMF Tab. 2]`, `[OCMF §Signing and Verification]` |
//! | All records share one pagination context | `[OCMF Tab. 2]` |
//! | The session opens with `TX=B` and closes with an ending marker | `[OCMF §Signing and Verification]` |
//! | Every record names the same signing component | `[OCMF §Relation of Serial Numbers]` |
//! | Readings advance in time | `[OCMF Tab. 7, TM]` |
//! | The billed register runs forward | a register that runs backwards is a fault |
//! | Only `ST=G` readings without an energy error flag may be billed | `[OCMF Tab. 10]`, `[MessEG §33]` |
//! | Billed energy is `end − start` on one OBIS register | `[OCMF Tab. 7]` |

use std::collections::BTreeSet;

use emob_core::Energy;
use rust_decimal::Decimal;

use crate::ocmf::{
    MeterState, OcmfRecord, Pagination, PaginationContext, ReadingUnit, TransactionMarker,
};

/// Something that stands between a set of records and an invoice.
///
/// Every variant carries enough context to act on: which record, which value,
/// what was expected. A finding that says only "invalid chain" is a support
/// ticket that will be answered with "please send the records".
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChainFinding {
    /// No records at all.
    Empty,

    /// The pagination counter skipped or repeated: a record is missing or
    /// duplicated.
    PaginationBreak {
        /// The counter of the record before the break.
        after: u64,
        /// The counter that followed it.
        found: u64,
    },

    /// Records from two different pagination contexts were mixed.
    MixedPaginationContexts,

    /// The chain does not open with `TX=B`.
    NoBeginMarker,

    /// The chain does not close with an ending marker.
    NoEndMarker,

    /// Two records name different signing components.
    SigningComponentChanged {
        /// The serial the chain started with.
        expected: String,
        /// The serial that turned up.
        found: String,
    },

    /// A reading is older than the one before it.
    TimeWentBackwards {
        /// The earlier reading's raw `TM`.
        previous: String,
        /// The out-of-order reading's raw `TM`.
        found: String,
    },

    /// The meter was not in a billable state.
    MeterNotBillable {
        /// The pagination counter of the record.
        pagination: u64,
        /// The state that was reported.
        state: MeterState,
    },

    /// A fault flagged the energy value as unusable.
    EnergyFlaggedUnusable {
        /// The pagination counter of the record.
        pagination: u64,
    },

    /// A flag this build does not recognise was set. Treated as disqualifying:
    /// a future OCMF revision must not be able to widen what gets billed by
    /// adding a character an old implementation ignores.
    UnknownErrorFlag {
        /// The pagination counter of the record.
        pagination: u64,
        /// The characters that were not understood.
        flags: Vec<char>,
    },

    /// The register ran backwards between the begin and end readings.
    RegisterRanBackwards {
        /// The opening value.
        start: Decimal,
        /// The closing value.
        end: Decimal,
    },

    /// The begin and end readings are on different OBIS registers, so their
    /// difference is not an energy.
    ObisMismatch {
        /// The OBIS code at the start.
        start: String,
        /// The OBIS code at the end.
        end: String,
    },

    /// No usable energy reading pair was found.
    NoBillableEnergy,

    /// The unit is not an energy unit.
    NotAnEnergyUnit {
        /// What the reading claimed.
        unit: Option<ReadingUnit>,
    },
}

impl ChainFinding {
    /// Whether this finding forbids billing.
    ///
    /// Every finding currently does. The distinction is kept because the
    /// Eichrecht reform in flight (MID Annex Va) is expected to introduce
    /// tolerances, and a report that already separates "wrong" from "worth
    /// noting" will not have to be redesigned to express them.
    #[must_use]
    pub const fn is_blocking(&self) -> bool {
        true
    }
}

impl core::fmt::Display for ChainFinding {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => write!(f, "no records were supplied for this session"),
            Self::PaginationBreak { after, found } => write!(
                f,
                "pagination jumped from {after} to {found}: a record is missing or duplicated"
            ),
            Self::MixedPaginationContexts => {
                write!(
                    f,
                    "records from the transaction and fiscal contexts were mixed"
                )
            }
            Self::NoBeginMarker => write!(f, "the chain does not open with TX=B"),
            Self::NoEndMarker => write!(f, "the chain does not close with an ending marker"),
            Self::SigningComponentChanged { expected, found } => write!(
                f,
                "the signing component changed mid-session: {expected} then {found}"
            ),
            Self::TimeWentBackwards { previous, found } => {
                write!(f, "time went backwards: {previous} then {found}")
            }
            Self::MeterNotBillable { pagination, state } => write!(
                f,
                "the meter was in state {state:?} at record {pagination}, which may not be billed"
            ),
            Self::EnergyFlaggedUnusable { pagination } => write!(
                f,
                "record {pagination} flags its energy value as unusable (EF contains 'E')"
            ),
            Self::UnknownErrorFlag { pagination, flags } => write!(
                f,
                "record {pagination} carries error flags this build does not know: {flags:?}"
            ),
            Self::RegisterRanBackwards { start, end } => {
                write!(f, "the register ran backwards: {start} then {end}")
            }
            Self::ObisMismatch { start, end } => write!(
                f,
                "the opening and closing readings are on different registers: {start} and {end}"
            ),
            Self::NoBillableEnergy => write!(f, "no usable pair of energy readings"),
            Self::NotAnEnergyUnit { unit } => {
                write!(f, "the reading is not in an energy unit: {unit:?}")
            }
        }
    }
}

/// What a chain of records adds up to.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainReport {
    /// Everything standing between these records and an invoice.
    pub findings: Vec<ChainFinding>,
    /// The energy the session may be billed for, when nothing blocks it.
    ///
    /// `None` whenever any finding is blocking. This is the crate's central
    /// promise: **a value that does not verify does not bill**, and the type
    /// system carries it rather than a convention.
    pub billable_energy: Option<Energy>,
    /// The OBIS register the energy was taken from.
    pub register: Option<String>,
    /// The signing component the chain belongs to.
    pub signing_component: Option<String>,
    /// When the session began.
    pub started_at: Option<time::OffsetDateTime>,
    /// When it ended.
    pub ended_at: Option<time::OffsetDateTime>,
}

impl ChainReport {
    /// Whether the session may be billed.
    #[must_use]
    pub fn is_billable(&self) -> bool {
        self.billable_energy.is_some()
    }

    /// The findings that block billing.
    pub fn blocking(&self) -> impl Iterator<Item = &ChainFinding> {
        self.findings.iter().filter(|f| f.is_blocking())
    }
}

/// Validate a chain of parsed records, in the order they were produced.
///
/// The records must already have had their signatures verified — this function
/// deliberately does not do that, because the key registry is I/O and this
/// crate's domain half stays pure. [`crate::Evidence`] wires the two together.
///
/// ```
/// use emob_eichrecht::{chain, ocmf};
/// # let begin_raw = r#"OCMF|{"PG":"T1","MS":"M1","RD":[{"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":10.0,"RI":"01-00:B2.08.00*FF","RU":"kWh","ST":"G"}]}|{"SD":"00"}"#;
/// # let end_raw = r#"OCMF|{"PG":"T2","MS":"M1","RD":[{"TM":"2026-01-02T11:00:00,000+0100 S","TX":"E","RV":25.0,"RI":"01-00:B2.08.00*FF","RU":"kWh","ST":"G"}]}|{"SD":"00"}"#;
///
/// let begin = ocmf::parse(begin_raw)?;
/// let end = ocmf::parse(end_raw)?;
/// let report = chain::validate(&[begin, end]);
///
/// if let Some(energy) = report.billable_energy {
///     println!("bill {energy}");
/// } else {
///     for finding in report.blocking() {
///         eprintln!("blocked: {finding}");
///     }
/// }
/// # Ok::<(), emob_eichrecht::error::OcmfError>(())
/// ```
#[must_use]
pub fn validate(records: &[OcmfRecord]) -> ChainReport {
    let mut findings = Vec::new();

    if records.is_empty() {
        return ChainReport {
            findings: vec![ChainFinding::Empty],
            billable_energy: None,
            register: None,
            signing_component: None,
            started_at: None,
            ended_at: None,
        };
    }

    check_pagination(records, &mut findings);
    let signing_component = check_signing_component(records, &mut findings);
    check_markers(records, &mut findings);
    check_time_order(records, &mut findings);
    check_reading_states(records, &mut findings);

    let (energy, register, started_at, ended_at) = compute_energy(records, &mut findings);

    // The gate. Any blocking finding and there is no number, whatever the
    // arithmetic produced — moving money on a chain that does not hold up is
    // the one thing this crate exists to prevent.
    let billable_energy = if findings.iter().any(ChainFinding::is_blocking) {
        None
    } else {
        energy
    };

    ChainReport {
        findings,
        billable_energy,
        register,
        signing_component,
        started_at,
        ended_at,
    }
}

fn check_pagination(records: &[OcmfRecord], findings: &mut Vec<ChainFinding>) {
    let contexts: BTreeSet<PaginationContext> = records
        .iter()
        .map(|r| r.payload.pagination.context)
        .collect();
    if contexts.len() > 1 {
        findings.push(ChainFinding::MixedPaginationContexts);
    }

    for pair in records.windows(2) {
        let Pagination { number: a, .. } = pair[0].payload.pagination;
        let Pagination { number: b, .. } = pair[1].payload.pagination;
        if b != a + 1 {
            findings.push(ChainFinding::PaginationBreak { after: a, found: b });
        }
    }
}

fn check_signing_component(
    records: &[OcmfRecord],
    findings: &mut Vec<ChainFinding>,
) -> Option<String> {
    let first = records[0].payload.signing_component_serial()?.to_owned();
    for record in &records[1..] {
        if let Some(serial) = record.payload.signing_component_serial()
            && serial != first
        {
            findings.push(ChainFinding::SigningComponentChanged {
                expected: first.clone(),
                found: serial.to_owned(),
            });
            break;
        }
    }
    Some(first)
}

fn check_markers(records: &[OcmfRecord], findings: &mut Vec<ChainFinding>) {
    // Fiscal readings carry no transaction reference at all, so the
    // begin/end rule does not apply to them.
    if records[0].payload.pagination.context == PaginationContext::Fiscal {
        return;
    }
    let opens = records
        .first()
        .and_then(|r| r.payload.readings.first())
        .and_then(|r| r.transaction)
        .is_some_and(TransactionMarker::begins_transaction);
    if !opens {
        findings.push(ChainFinding::NoBeginMarker);
    }

    let closes = records
        .last()
        .and_then(|r| r.payload.readings.last())
        .and_then(|r| r.transaction)
        .is_some_and(TransactionMarker::ends_transaction);
    if !closes {
        findings.push(ChainFinding::NoEndMarker);
    }
}

fn check_time_order(records: &[OcmfRecord], findings: &mut Vec<ChainFinding>) {
    let mut previous: Option<&crate::ocmf::OcmfTime> = None;
    for record in records {
        for reading in &record.payload.readings {
            if let Some(prev) = previous
                && reading.time.instant < prev.instant
            {
                findings.push(ChainFinding::TimeWentBackwards {
                    previous: prev.raw.clone(),
                    found: reading.time.raw.clone(),
                });
                return;
            }
            previous = Some(&reading.time);
        }
    }
}

fn check_reading_states(records: &[OcmfRecord], findings: &mut Vec<ChainFinding>) {
    for record in records {
        let pagination = record.payload.pagination.number;
        for reading in &record.payload.readings {
            if !reading.state.is_billable() {
                findings.push(ChainFinding::MeterNotBillable {
                    pagination,
                    state: reading.state,
                });
            }
            if reading.error_flags.energy_unusable {
                findings.push(ChainFinding::EnergyFlaggedUnusable { pagination });
            }
            if !reading.unknown_error_flags.is_empty() {
                findings.push(ChainFinding::UnknownErrorFlag {
                    pagination,
                    flags: reading.unknown_error_flags.clone(),
                });
            }
        }
    }
}

type EnergyOutcome = (
    Option<Energy>,
    Option<String>,
    Option<time::OffsetDateTime>,
    Option<time::OffsetDateTime>,
);

fn compute_energy(records: &[OcmfRecord], findings: &mut Vec<ChainFinding>) -> EnergyOutcome {
    let begin = records.iter().find_map(|r| r.payload.begin_reading());
    let end = records.iter().rev().find_map(|r| r.payload.end_reading());

    let (Some(begin), Some(end)) = (begin, end) else {
        findings.push(ChainFinding::NoBillableEnergy);
        return (None, None, None, None);
    };

    let started_at = Some(begin.time.instant);
    let ended_at = Some(end.time.instant);

    // The registers have to be the same one, or the subtraction is between two
    // different quantities and means nothing.
    match (begin.obis.as_deref(), end.obis.as_deref()) {
        (Some(a), Some(b)) if a != b => {
            findings.push(ChainFinding::ObisMismatch {
                start: a.to_owned(),
                end: b.to_owned(),
            });
            return (None, None, started_at, ended_at);
        }
        _ => {}
    }

    if !begin.unit.is_some_and(ReadingUnit::is_energy) {
        findings.push(ChainFinding::NotAnEnergyUnit { unit: begin.unit });
        return (None, None, started_at, ended_at);
    }

    let (Some(start_value), Some(end_value)) = (begin.value, end.value) else {
        findings.push(ChainFinding::NoBillableEnergy);
        return (None, None, started_at, ended_at);
    };

    if end_value < start_value {
        findings.push(ChainFinding::RegisterRanBackwards {
            start: start_value,
            end: end_value,
        });
        return (None, None, started_at, ended_at);
    }

    // The subtraction OCMF prescribes, done exactly, with the meter's own
    // scale preserved on both sides.
    let difference = end_value - start_value;
    let energy = match begin.unit {
        Some(ReadingUnit::KWh) => Energy::from_kwh(difference).ok(),
        Some(ReadingUnit::Wh) => Energy::from_wh(difference).ok(),
        _ => None,
    };

    (energy, begin.obis.clone(), started_at, ended_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocmf;

    fn record(pg: u64, tx: &str, value: &str, state: &str, minute: u8) -> OcmfRecord {
        let raw = format!(
            r#"OCMF|{{"PG":"T{pg}","MS":"BQ1","RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{value},"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"{state}"}}]}}|{{"SD":"00"}}"#
        );
        ocmf::parse(&raw).unwrap()
    }

    fn good_session() -> Vec<OcmfRecord> {
        vec![
            record(1, "B", "2935.600", "G", 0),
            record(2, "C", "2950.000", "G", 10),
            record(3, "E", "2965.100", "G", 20),
        ]
    }

    #[test]
    fn a_clean_session_bills_the_difference() {
        let report = validate(&good_session());
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert_eq!(
            report.billable_energy.unwrap().to_string(),
            "29.500 kWh",
            "the scale the meter stated survives into the invoice"
        );
        assert_eq!(report.register.as_deref(), Some("01-00:B2.08.00*FF"));
        assert_eq!(report.signing_component.as_deref(), Some("BQ1"));
        assert!(report.is_billable());
    }

    #[test]
    fn deleting_a_record_is_caught_though_every_signature_still_holds() {
        // This is the attack signature checking cannot see.
        let session = vec![
            record(1, "B", "2935.600", "G", 0),
            record(3, "E", "2965.100", "G", 20),
        ];
        let report = validate(&session);
        assert!(
            report
                .findings
                .contains(&ChainFinding::PaginationBreak { after: 1, found: 3 })
        );
        assert!(
            !report.is_billable(),
            "a session with a hole in it does not bill"
        );
    }

    #[test]
    fn a_duplicated_record_is_caught_too() {
        let session = vec![
            record(1, "B", "2935.600", "G", 0),
            record(1, "B", "2935.600", "G", 0),
            record(2, "E", "2965.100", "G", 20),
        ];
        let report = validate(&session);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, ChainFinding::PaginationBreak { .. }))
        );
        assert!(!report.is_billable());
    }

    #[test]
    fn a_substitute_value_blocks_the_invoice() {
        let session = vec![
            record(1, "B", "2935.600", "G", 0),
            record(2, "C", "2950.000", "S", 10), // substitute
            record(3, "E", "2965.100", "G", 20),
        ];
        let report = validate(&session);
        assert!(report.findings.contains(&ChainFinding::MeterNotBillable {
            pagination: 2,
            state: MeterState::Substitute,
        }));
        assert!(!report.is_billable());
    }

    #[test]
    fn a_manipulated_meter_blocks_the_invoice() {
        let session = vec![
            record(1, "B", "2935.600", "M", 0),
            record(2, "E", "2965.100", "G", 20),
        ];
        let report = validate(&session);
        assert!(!report.is_billable());
    }

    #[test]
    fn an_energy_error_flag_blocks_the_invoice() {
        let raw = r#"OCMF|{"PG":"T2","MS":"BQ1","RD":[{"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":2965.100,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"E","ST":"G"}]}|{"SD":"00"}"#;
        let session = vec![
            record(1, "B", "2935.600", "G", 0),
            ocmf::parse(raw).unwrap(),
        ];
        let report = validate(&session);
        assert!(
            report
                .findings
                .contains(&ChainFinding::EnergyFlaggedUnusable { pagination: 2 })
        );
        assert!(!report.is_billable());
    }

    #[test]
    fn an_unknown_error_flag_blocks_rather_than_being_ignored() {
        // A future OCMF revision must not be able to widen what gets billed by
        // adding a character this build skips over.
        let raw = r#"OCMF|{"PG":"T2","MS":"BQ1","RD":[{"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":2965.100,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"Q","ST":"G"}]}|{"SD":"00"}"#;
        let session = vec![
            record(1, "B", "2935.600", "G", 0),
            ocmf::parse(raw).unwrap(),
        ];
        let report = validate(&session);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, ChainFinding::UnknownErrorFlag { .. }))
        );
        assert!(!report.is_billable());
    }

    #[test]
    fn a_missing_begin_marker_is_caught() {
        let session = vec![
            record(1, "C", "2935.600", "G", 0),
            record(2, "E", "2965.100", "G", 20),
        ];
        let report = validate(&session);
        assert!(report.findings.contains(&ChainFinding::NoBeginMarker));
    }

    #[test]
    fn a_missing_end_marker_is_caught() {
        let session = vec![
            record(1, "B", "2935.600", "G", 0),
            record(2, "C", "2965.100", "G", 20),
        ];
        let report = validate(&session);
        assert!(report.findings.contains(&ChainFinding::NoEndMarker));
    }

    #[test]
    fn a_register_running_backwards_is_a_fault_not_a_credit() {
        let session = vec![
            record(1, "B", "2965.100", "G", 0),
            record(2, "E", "2935.600", "G", 20),
        ];
        let report = validate(&session);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, ChainFinding::RegisterRanBackwards { .. }))
        );
        assert!(!report.is_billable());
    }

    #[test]
    fn a_swapped_signing_component_is_caught() {
        let other = ocmf::parse(
            r#"OCMF|{"PG":"T2","MS":"OTHER","RD":[{"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":2965.100,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}]}|{"SD":"00"}"#,
        )
        .unwrap();
        let report = validate(&[record(1, "B", "2935.600", "G", 0), other]);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, ChainFinding::SigningComponentChanged { .. }))
        );
    }

    #[test]
    fn time_running_backwards_is_caught() {
        let session = vec![
            record(1, "B", "2935.600", "G", 30),
            record(2, "E", "2965.100", "G", 10),
        ];
        let report = validate(&session);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, ChainFinding::TimeWentBackwards { .. }))
        );
    }

    #[test]
    fn readings_on_different_registers_do_not_subtract() {
        let end = ocmf::parse(
            r#"OCMF|{"PG":"T2","MS":"BQ1","RD":[{"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":2965.100,"RI":"01-00:B3.08.00*FF","RU":"kWh","EF":"","ST":"G"}]}|{"SD":"00"}"#,
        )
        .unwrap();
        let report = validate(&[record(1, "B", "2935.600", "G", 0), end]);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, ChainFinding::ObisMismatch { .. }))
        );
        assert!(!report.is_billable());
    }

    #[test]
    fn an_empty_chain_is_a_finding_not_a_panic() {
        let report = validate(&[]);
        assert_eq!(report.findings, vec![ChainFinding::Empty]);
        assert!(!report.is_billable());
    }

    #[test]
    fn one_record_carrying_both_markers_is_a_whole_session() {
        // The `MR` (MultipleReading) configuration: start and stop in a single
        // OCMF data set [OCMF §Best Practice].
        let raw = r#"OCMF|{"PG":"T1","MS":"BQ1","RD":[
            {"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":2935.600,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"},
            {"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":2965.100,"EF":"","ST":"G"}
        ]}|{"SD":"00"}"#;
        let report = validate(&[ocmf::parse(raw).unwrap()]);
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert_eq!(report.billable_energy.unwrap().to_string(), "29.500 kWh");
    }

    #[test]
    fn wh_registers_convert_to_kwh() {
        let raw = r#"OCMF|{"PG":"T1","MS":"BQ1","RD":[
            {"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":1000,"RI":"01-00:B2.08.00*FF","RU":"Wh","EF":"","ST":"G"},
            {"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":30500,"EF":"","ST":"G"}
        ]}|{"SD":"00"}"#;
        let report = validate(&[ocmf::parse(raw).unwrap()]);
        // 29500 Wh, so 29.500 kWh: the Wh register's resolution survives.
        assert_eq!(report.billable_energy.unwrap().kwh().to_string(), "29.500");
    }

    #[test]
    fn mixed_contexts_are_refused() {
        let fiscal = ocmf::parse(
            r#"OCMF|{"PG":"F2","MS":"BQ1","RD":[{"TM":"2026-01-02T10:20:00,000+0100 S","RV":2965.100,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}]}|{"SD":"00"}"#,
        )
        .unwrap();
        let report = validate(&[record(1, "B", "2935.600", "G", 0), fiscal]);
        assert!(
            report
                .findings
                .contains(&ChainFinding::MixedPaginationContexts)
        );
    }

    #[test]
    fn every_finding_renders_a_useful_message() {
        for finding in [
            ChainFinding::Empty,
            ChainFinding::PaginationBreak { after: 1, found: 3 },
            ChainFinding::MixedPaginationContexts,
            ChainFinding::NoBeginMarker,
            ChainFinding::NoEndMarker,
            ChainFinding::NoBillableEnergy,
        ] {
            let text = finding.to_string();
            assert!(text.len() > 10, "{finding:?} renders as {text:?}");
        }
    }
}
