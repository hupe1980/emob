//! The OCMF payload, typed.
//!
//! Field names follow the specification's two-letter keys `[OCMF Tab. 1–8]`
//! rather than being spelled out, because every implementation, every captured
//! sample and every conversation with a station vendor uses them. A reader
//! holding this file beside `specs/eichrecht/ocmf-x/OCMF-en.md` should be able
//! to match them line for line.

use rust_decimal::Decimal;

use super::obis::ObisCode;
use crate::error::OcmfError;

/// The pagination context: is this reading part of a transaction, or a
/// standalone fiscal reading?
///
/// The transaction context `T` is mandatory for a signature component; the
/// fiscal context `F` is optional and carries no transaction reference
/// `[OCMF Tab. 2]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PaginationContext {
    /// `T` — readings in transaction reference.
    Transaction,
    /// `F` — readings independent of transactions ("fiscal metering").
    Fiscal,
}

/// A parsed `PG` field: a context and a counter.
///
/// The counter increments by exactly one per record within its context, which
/// is what makes a missing record detectable. A verifier that checks signatures
/// but not pagination will happily accept a session whose middle was deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Pagination {
    /// Which counter this belongs to.
    pub context: PaginationContext,
    /// The counter value.
    pub number: u64,
}

impl Pagination {
    /// Parse a `PG` value such as `T12345`.
    ///
    /// # Errors
    ///
    /// [`OcmfError::BadPagination`] when the indicator is unknown or the number
    /// is missing, non-numeric, or carries a leading zero (the specification
    /// says "a number without leading zeros", and accepting `T007` alongside
    /// `T7` would make two spellings of one counter value).
    pub fn parse(raw: &str) -> Result<Self, OcmfError> {
        let mut chars = raw.chars();
        let context = match chars.next() {
            Some('T') => PaginationContext::Transaction,
            Some('F') => PaginationContext::Fiscal,
            _ => {
                return Err(OcmfError::BadPagination {
                    value: raw.to_owned(),
                });
            }
        };
        let digits = chars.as_str();
        if digits.is_empty()
            || !digits.bytes().all(|b| b.is_ascii_digit())
            || (digits.len() > 1 && digits.starts_with('0'))
        {
            return Err(OcmfError::BadPagination {
                value: raw.to_owned(),
            });
        }
        let number = digits
            .parse::<u64>()
            .map_err(|_| OcmfError::BadPagination {
                value: raw.to_owned(),
            })?;
        Ok(Self { context, number })
    }
}

impl core::fmt::Display for Pagination {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let indicator = match self.context {
            PaginationContext::Transaction => 'T',
            PaginationContext::Fiscal => 'F',
        };
        write!(f, "{indicator}{}", self.number)
    }
}

/// The state of the meter at the moment of a reading `[OCMF Tab. 10]`.
///
/// Exactly one of these applies to a reading. Only [`MeterState::Ok`] describes
/// a value that may be billed; every other variant is a fault, a substitute or
/// a manipulation, and [`MeterState::is_billable`] is the single place that
/// judgement is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum MeterState {
    /// `N` — the meter is not present or was not found.
    NotPresent,
    /// `G` — the meter is working correctly.
    Ok,
    /// `T` — timeout while controlling the meter.
    Timeout,
    /// `D` — the meter was disconnected from the signature component.
    Disconnected,
    /// `R` — the meter is no longer found, having been seen before.
    NotFound,
    /// `M` — manipulation detected.
    Manipulated,
    /// `X` — the meter was exchanged; the serial no longer matches.
    Exchanged,
    /// `I` — meter or API version incompatible with the signature component.
    Incompatible,
    /// `O` — the read value is outside the meter's value range.
    OutOfRange,
    /// `S` — a substitute value was formed.
    Substitute,
    /// `E` — another, unspecified error.
    OtherError,
    /// `F` — the register was not read correctly; the value is not valid.
    ReadError,
}

impl MeterState {
    /// Parse the single-letter code.
    ///
    /// # Errors
    ///
    /// [`OcmfError::UnknownMeterState`] for a code outside Table 10. Unknown is
    /// *not* treated as `Ok`: a signature component reporting a state this
    /// crate has never heard of is exactly the case where billing must stop.
    pub fn parse(code: &str) -> Result<Self, OcmfError> {
        Ok(match code {
            "N" => Self::NotPresent,
            "G" => Self::Ok,
            "T" => Self::Timeout,
            "D" => Self::Disconnected,
            "R" => Self::NotFound,
            "M" => Self::Manipulated,
            "X" => Self::Exchanged,
            "I" => Self::Incompatible,
            "O" => Self::OutOfRange,
            "S" => Self::Substitute,
            "E" => Self::OtherError,
            "F" => Self::ReadError,
            other => {
                return Err(OcmfError::UnknownMeterState {
                    code: other.to_owned(),
                });
            }
        })
    }

    /// Whether a reading in this state may be used for billing.
    ///
    /// Only `G` (OK) may. A substitute value is a value the meter invented
    /// because it could not measure — perfectly legitimate for operational
    /// telemetry, and never a basis for an invoice under `[MessEG §33]`.
    #[must_use]
    pub const fn is_billable(self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Why a reading was taken, and where it sits in a transaction
/// `[OCMF Tab. 7, TX]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum TransactionMarker {
    /// `B` — beginning of the transaction.
    Begin,
    /// `C` — during charging.
    Charging,
    /// `X` — error during charging; time and/or energy are unusable from here.
    Exception,
    /// `E` — end of transaction.
    End,
    /// `L` — ended locally.
    EndedLocally,
    /// `R` — ended remotely.
    EndedRemotely,
    /// `A` — aborted by an error.
    Aborted,
    /// `P` — ended by a power failure.
    PowerFailure,
    /// `S` — suspended: the transaction is active but not charging.
    Suspended,
    /// `T` — a tariff change.
    TariffChange,
}

impl TransactionMarker {
    /// Parse the single-letter code.
    ///
    /// # Errors
    ///
    /// [`OcmfError::UnknownTransactionMarker`] for a code outside Table 7.
    pub fn parse(code: &str) -> Result<Self, OcmfError> {
        Ok(match code {
            "B" => Self::Begin,
            "C" => Self::Charging,
            "X" => Self::Exception,
            "E" => Self::End,
            "L" => Self::EndedLocally,
            "R" => Self::EndedRemotely,
            "A" => Self::Aborted,
            "P" => Self::PowerFailure,
            "S" => Self::Suspended,
            "T" => Self::TariffChange,
            other => {
                return Err(OcmfError::UnknownTransactionMarker {
                    code: other.to_owned(),
                });
            }
        })
    }

    /// Whether this marker closes a transaction.
    ///
    /// `E`, `L`, `R`, `A` and `P` all end one; the last three say *how*, which
    /// matters for the dispute but not for the arithmetic.
    #[must_use]
    pub const fn ends_transaction(self) -> bool {
        matches!(
            self,
            Self::End
                | Self::EndedLocally
                | Self::EndedRemotely
                | Self::Aborted
                | Self::PowerFailure
        )
    }

    /// Whether this marker opens a transaction.
    #[must_use]
    pub const fn begins_transaction(self) -> bool {
        matches!(self, Self::Begin)
    }
}

/// How trustworthy the clock behind a reading is `[OCMF Tab. 19]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum TimeStatus {
    /// `U` — unknown, unsynchronised.
    Unknown,
    /// `I` — informative only.
    Informative,
    /// `S` — synchronised.
    Synchronized,
    /// `R` — relative accounting from a calibration-law-accurate duration on
    /// top of an informative start time.
    Relative,
}

impl TimeStatus {
    /// Parse the single-letter code.
    ///
    /// # Errors
    ///
    /// [`OcmfError::UnknownTimeStatus`] for a code outside Table 19.
    pub fn parse(code: &str) -> Result<Self, OcmfError> {
        Ok(match code {
            "U" => Self::Unknown,
            "I" => Self::Informative,
            "S" => Self::Synchronized,
            "R" => Self::Relative,
            other => {
                return Err(OcmfError::UnknownTimeStatus {
                    code: other.to_owned(),
                });
            }
        })
    }

    /// Whether a time-based tariff may be billed against this clock.
    ///
    /// `S` and `R` qualify; `U` and `I` do not. A time-priced session billed
    /// off an unsynchronised clock is billing a duration nobody can defend.
    /// Energy-priced sessions are unaffected — that is what the register is for.
    #[must_use]
    pub const fn is_billable_for_time(self) -> bool {
        matches!(self, Self::Synchronized | Self::Relative)
    }
}

/// A timestamp plus the synchronisation state that qualifies it.
///
/// OCMF writes these as one string, `"2018-07-24T13:22:04,000+0200 S"` — an
/// ISO 8601 instant with a comma for the decimal separator, a space, and a
/// status letter. They are kept together here because using one without the
/// other is the mistake the format is shaped to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OcmfTime {
    /// The instant, parsed.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub instant: time::OffsetDateTime,
    /// How much the clock behind it can be trusted.
    pub status: TimeStatus,
    /// The field exactly as it arrived, for the evidence record.
    pub raw: String,
}

/// Which quantities a fault has made unusable `[OCMF Tab. 7, EF]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ErrorFlags {
    /// `E` — the energy value is no longer usable for billing.
    pub energy_unusable: bool,
    /// `t` — the time value is no longer usable for billing.
    pub time_unusable: bool,
}

impl ErrorFlags {
    /// Parse the `EF` string. An empty string means no errors.
    ///
    /// Unknown characters are *kept* as a flag rather than ignored: a future
    /// OCMF revision adding a third quantity must not silently read as "no
    /// error" in an implementation written before it.
    #[must_use]
    pub fn parse(raw: &str) -> (Self, Vec<char>) {
        let mut flags = Self::default();
        let mut unknown = Vec::new();
        for c in raw.chars() {
            match c {
                'E' => flags.energy_unusable = true,
                't' => flags.time_unusable = true,
                other => unknown.push(other),
            }
        }
        (flags, unknown)
    }

    /// Whether any quantity was flagged.
    #[must_use]
    pub const fn any(self) -> bool {
        self.energy_unusable || self.time_unusable
    }
}

/// The unit of a reading `[OCMF Tab. 20]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum ReadingUnit {
    /// Kilowatt-hours.
    KWh,
    /// Watt-hours.
    Wh,
    /// Milliohms — cable-loss compensation only.
    MOhm,
    /// Microohms — cable-loss compensation only.
    UOhm,
}

impl ReadingUnit {
    /// Parse the unit string.
    ///
    /// # Errors
    ///
    /// [`OcmfError::UnknownUnit`] for a unit outside Table 20.
    pub fn parse(raw: &str) -> Result<Self, OcmfError> {
        Ok(match raw {
            "kWh" => Self::KWh,
            "Wh" => Self::Wh,
            "mOhm" => Self::MOhm,
            "uOhm" => Self::UOhm,
            other => {
                return Err(OcmfError::UnknownUnit {
                    unit: other.to_owned(),
                });
            }
        })
    }

    /// Whether this unit measures energy at all.
    #[must_use]
    pub const fn is_energy(self) -> bool {
        matches!(self, Self::KWh | Self::Wh)
    }

    /// Whether this unit measures resistance — the two `LU` may take
    /// `[OCMF Tab. 24]`.
    #[must_use]
    pub const fn is_resistance(self) -> bool {
        matches!(self, Self::MOhm | Self::UOhm)
    }
}

/// Alternating or direct current `[OCMF Tab. 21]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CurrentType {
    /// Alternating current.
    Ac,
    /// Direct current.
    Dc,
}

/// One meter reading inside a record `[OCMF Tab. 7]`.
///
/// Fields the specification allows to be omitted "when identical to the
/// previous reading" are already resolved here: the parser carries the last
/// seen value forward, so a consumer never has to know which readings were
/// abbreviated on the wire.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Reading {
    /// `TM` — when, and how good the clock was.
    pub time: OcmfTime,
    /// `TX` — why, and where in the transaction. Absent for fiscal readings.
    pub transaction: Option<TransactionMarker>,
    /// `RV` — the value, exact, with the scale the meter stated.
    pub value: Option<Decimal>,
    /// `RI` — the OBIS code identifying what was read, classified as far as it
    /// can be `[OCMF Tab. 25]`.
    pub obis: Option<ObisCode>,
    /// `RU` — the unit.
    pub unit: Option<ReadingUnit>,
    /// `RT` — AC or DC.
    pub current_type: Option<CurrentType>,
    /// `CL` — cumulated cable loss already deducted from `RV`.
    pub cumulated_loss: Option<Decimal>,
    /// `EF` — which quantities a fault has made unusable.
    pub error_flags: ErrorFlags,
    /// Characters in `EF` this version does not know.
    pub unknown_error_flags: Vec<char>,
    /// `ST` — the meter's state.
    pub state: MeterState,
}

impl Reading {
    /// The register value as an [`emob_core::Energy`], in the unit the reading
    /// states.
    ///
    /// `None` for a reading with no value — `[OCMF Tab. 7]` lets `RV` be
    /// omitted "if only the occurrence of an error condition (event) of the
    /// meter is to be indicated" — or one whose unit is not an energy at all.
    ///
    /// The conversion lives here rather than at each call site because `RU` is
    /// `Wh` on ordinary German hardware and kWh elsewhere, and a caller that
    /// reads `value` and forgets `unit` is out by a factor of a thousand.
    /// [`Energy::from_wh`] shifts the decimal point rather than dividing, so
    /// the meter's own resolution survives either way.
    ///
    /// [`Energy::from_wh`]: emob_core::Energy::from_wh
    #[must_use]
    pub fn energy(&self) -> Option<emob_core::Energy> {
        let value = self.value?;
        match self.unit? {
            ReadingUnit::KWh => emob_core::Energy::from_kwh(value).ok(),
            ReadingUnit::Wh => emob_core::Energy::from_wh(value).ok(),
            ReadingUnit::MOhm | ReadingUnit::UOhm => None,
        }
    }

    /// Whether this reading may be used as the basis for an energy charge.
    ///
    /// Three conditions, all of which have to hold: the meter was working, no
    /// fault flagged the energy, and there is a value in an energy unit.
    #[must_use]
    pub fn is_billable_energy(&self) -> bool {
        self.state.is_billable()
            && !self.error_flags.energy_unusable
            && self.unknown_error_flags.is_empty()
            && self.value.is_some()
            && self.unit.is_some_and(ReadingUnit::is_energy)
    }
}

/// How a user was identified `[OCMF Tab. 11]`.
///
/// The ordering is meaningful: `Hearsay` (a bare RFID UID) is weaker than
/// `Trusted` (backend authorisation), which is weaker than `Secure` (Plug &
/// Charge). The error states are separate and never compare as "some
/// assignment".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum IdentificationLevel {
    /// No user assignment; the other fields carry no meaning.
    None,
    /// Unsecured, e.g. a plain RFID UID.
    Hearsay,
    /// Trustworthy to a degree, e.g. backend authorisation.
    Trusted,
    /// Verified by the signature component and special measures.
    Verified,
    /// Verified by a cryptographic signature certifying the assignment.
    Certified,
    /// Established by a secure feature — a secure RFID card, ISO 15118
    /// Plug & Charge.
    Secure,
    /// Error: the `UID`s do not match.
    Mismatch,
    /// Error: the certificate did not check out.
    Invalid,
    /// Error: the referenced trust certificate has expired.
    Outdated,
    /// Error: no matching trust certificate was found.
    Unknown,
}

impl IdentificationLevel {
    /// Parse the identifier.
    ///
    /// # Errors
    ///
    /// [`OcmfError::UnknownIdentificationLevel`] for a value outside Table 11.
    pub fn parse(raw: &str) -> Result<Self, OcmfError> {
        Ok(match raw {
            "NONE" => Self::None,
            "HEARSAY" => Self::Hearsay,
            "TRUSTED" => Self::Trusted,
            "VERIFIED" => Self::Verified,
            "CERTIFIED" => Self::Certified,
            "SECURE" => Self::Secure,
            "MISMATCH" => Self::Mismatch,
            "INVALID" => Self::Invalid,
            "OUTDATED" => Self::Outdated,
            "UNKNOWN" => Self::Unknown,
            other => {
                return Err(OcmfError::UnknownIdentificationLevel {
                    level: other.to_owned(),
                });
            }
        })
    }

    /// Whether this level reports an error rather than an assignment.
    #[must_use]
    pub const fn is_error(self) -> bool {
        matches!(
            self,
            Self::Mismatch | Self::Invalid | Self::Outdated | Self::Unknown
        )
    }
}

/// The cable-loss compensation parameters `[OCMF Tab. 24]`.
///
/// Present when the meter compensates for the resistance of the charging cable,
/// which is what lets a station bill the energy that reached the *vehicle*
/// rather than the energy that left the meter. `LR` and `LU` are mandatory
/// inside the object: a compensation nobody can reproduce is a compensation
/// nobody can check, and a notified body asks for exactly this.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LossCompensation {
    /// `LN` — a traceability text for the cable characteristics.
    pub naming: Option<String>,
    /// `LI` — a traceability id into the meter's documentation.
    pub identification: Option<Decimal>,
    /// `LR` — the cable resistance used in the computation. Mandatory.
    pub resistance: Decimal,
    /// `LU` — the unit of that resistance: milliohm or microohm. Mandatory.
    pub resistance_unit: ReadingUnit,
}

/// The identification section of a payload `[OCMF Tab. 4]`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Identification {
    /// `IS` — was a user assigned at all?
    pub assigned: bool,
    /// `IL` — how strong the assignment is.
    pub level: Option<IdentificationLevel>,
    /// `IF` — the detailed flags, kept as text: the tables are open-ended and
    /// a flag this crate does not know is still evidence.
    pub flags: Vec<String>,
    /// `IT` — the type of the identification data (`ISO14443`, `EMAID`, …).
    ///
    /// Mandatory whenever the section is present `[OCMF Tab. 4]`, so it is not
    /// an `Option`: a section without it is a malformed record, and the parser
    /// says so rather than substituting an empty string.
    pub id_type: String,
    /// `ID` — the identification data itself.
    pub id_data: Option<String>,
    /// `TT` — the tariff text, for the direct-payment case.
    pub tariff_text: Option<String>,
}

/// A complete OCMF payload — everything between the two pipes `[OCMF §Sections]`.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Payload {
    /// `FV` — format version.
    pub format_version: Option<String>,
    /// `GI` — gateway identification.
    pub gateway_id: Option<String>,
    /// `GS` — gateway serial. Conditionally mandatory.
    pub gateway_serial: Option<String>,
    /// `GV` — gateway software version.
    pub gateway_version: Option<String>,
    /// `PG` — pagination.
    pub pagination: Pagination,
    /// `MV` — meter vendor.
    pub meter_vendor: Option<String>,
    /// `MM` — meter model.
    pub meter_model: Option<String>,
    /// `MS` — meter serial.
    pub meter_serial: Option<String>,
    /// `MF` — meter firmware.
    pub meter_firmware: Option<String>,
    /// `CF` — charge controller firmware version.
    ///
    /// Some notified bodies require it, to tie an OCMF data set to the
    /// documentation of the corresponding Schalt-Mess-Koordination
    /// `[OCMF Tab. 5]`.
    pub controller_firmware: Option<String>,
    /// `LC` — the cable-loss compensation parameters, when the meter
    /// compensates.
    pub loss_compensation: Option<LossCompensation>,
    /// The user-assignment section, present exactly when there is a
    /// transaction reference.
    pub identification: Option<Identification>,
    /// `CT` — charge point identification type.
    pub charge_point_id_type: Option<String>,
    /// `CI` — charge point identification.
    pub charge_point_id: Option<String>,
    /// `RD` — the readings.
    pub readings: Vec<Reading>,
}

impl Payload {
    /// The serial number a public key is registered against.
    ///
    /// `[OCMF §Relation of Serial Numbers]`: the meter serial identifies the
    /// signing component when the meter signs; the gateway serial when a
    /// gateway does. Both may be present, and then both are needed to be
    /// unambiguous — so this returns the meter serial first and callers that
    /// need the pair read the fields.
    #[must_use]
    pub fn signing_component_serial(&self) -> Option<&str> {
        self.meter_serial
            .as_deref()
            .or(self.gateway_serial.as_deref())
    }

    /// The reading that opens the transaction, if this record contains one.
    #[must_use]
    pub fn begin_reading(&self) -> Option<&Reading> {
        self.readings.iter().find(|r| {
            r.transaction
                .is_some_and(TransactionMarker::begins_transaction)
        })
    }

    /// The reading that closes the transaction, if this record contains one.
    #[must_use]
    pub fn end_reading(&self) -> Option<&Reading> {
        self.readings.iter().find(|r| {
            r.transaction
                .is_some_and(TransactionMarker::ends_transaction)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagination_round_trips() {
        let p = Pagination::parse("T12345").unwrap();
        assert_eq!(p.context, PaginationContext::Transaction);
        assert_eq!(p.number, 12345);
        assert_eq!(p.to_string(), "T12345");

        assert_eq!(
            Pagination::parse("F1").unwrap().context,
            PaginationContext::Fiscal
        );
    }

    #[test]
    fn pagination_rejects_leading_zeros_and_junk() {
        // Two spellings of one counter value would break the "increments by
        // exactly one" check that makes deletion detectable.
        assert!(Pagination::parse("T007").is_err());
        assert!(Pagination::parse("T").is_err());
        assert!(Pagination::parse("Q1").is_err());
        assert!(Pagination::parse("T1a").is_err());
        assert!(Pagination::parse("").is_err());
        assert!(Pagination::parse("T0").is_ok(), "a bare zero is fine");
    }

    #[test]
    fn only_a_working_meter_is_billable() {
        assert!(MeterState::parse("G").unwrap().is_billable());
        for code in ["N", "T", "D", "R", "M", "X", "I", "O", "S", "E", "F"] {
            assert!(
                !MeterState::parse(code).unwrap().is_billable(),
                "{code} must not be billable"
            );
        }
    }

    #[test]
    fn a_substitute_value_is_not_a_measurement() {
        // The subtlest of the states: the meter produced a number, and it is
        // not one anybody may be invoiced for.
        assert_eq!(MeterState::parse("S").unwrap(), MeterState::Substitute);
        assert!(!MeterState::parse("S").unwrap().is_billable());
    }

    #[test]
    fn an_unknown_meter_state_is_an_error_not_an_ok() {
        assert!(MeterState::parse("Z").is_err());
    }

    #[test]
    fn every_ending_marker_ends_the_transaction() {
        for code in ["E", "L", "R", "A", "P"] {
            assert!(TransactionMarker::parse(code).unwrap().ends_transaction());
        }
        for code in ["B", "C", "S", "T", "X"] {
            assert!(!TransactionMarker::parse(code).unwrap().ends_transaction());
        }
        assert!(TransactionMarker::parse("B").unwrap().begins_transaction());
    }

    #[test]
    fn time_status_gates_only_time_billing() {
        assert!(TimeStatus::parse("S").unwrap().is_billable_for_time());
        assert!(TimeStatus::parse("R").unwrap().is_billable_for_time());
        assert!(!TimeStatus::parse("U").unwrap().is_billable_for_time());
        assert!(!TimeStatus::parse("I").unwrap().is_billable_for_time());
    }

    #[test]
    fn unknown_error_flags_are_kept_not_dropped() {
        let (flags, unknown) = ErrorFlags::parse("E");
        assert!(flags.energy_unusable);
        assert!(unknown.is_empty());

        let (flags, unknown) = ErrorFlags::parse("Et");
        assert!(flags.energy_unusable && flags.time_unusable);
        assert!(unknown.is_empty());

        // A future revision's flag must not read as "no error".
        let (flags, unknown) = ErrorFlags::parse("Q");
        assert!(!flags.any());
        assert_eq!(unknown, vec!['Q']);

        let (flags, unknown) = ErrorFlags::parse("");
        assert!(!flags.any());
        assert!(unknown.is_empty());
    }

    #[test]
    fn identification_errors_are_not_assignments() {
        assert!(IdentificationLevel::parse("MISMATCH").unwrap().is_error());
        assert!(IdentificationLevel::parse("INVALID").unwrap().is_error());
        assert!(!IdentificationLevel::parse("SECURE").unwrap().is_error());
        assert!(!IdentificationLevel::parse("HEARSAY").unwrap().is_error());
    }

    #[test]
    fn units_know_whether_they_are_energy() {
        assert!(ReadingUnit::parse("kWh").unwrap().is_energy());
        assert!(ReadingUnit::parse("Wh").unwrap().is_energy());
        assert!(!ReadingUnit::parse("mOhm").unwrap().is_energy());
        assert!(ReadingUnit::parse("MWh").is_err());
    }
}
