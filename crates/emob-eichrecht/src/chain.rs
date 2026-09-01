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
//! [`validate`] is that check component. It answers three questions — *may this
//! session's energy be billed, may its duration be billed, and to whom* — and
//! it answers them with a [`ChainReport`] that lists every reason it might not
//! be, rather than a bare boolean.
//!
//! # Three quantities, not one
//!
//! OCMF distinguishes them and so does this. A record carries `EF` flags for
//! energy (`E`) and time (`t`) separately `[OCMF Tab. 7, EF]`, states the
//! trustworthiness of its own clock separately again `[OCMF Tab. 19]`, and
//! states how the user was identified separately from both `[OCMF Tab. 11]`.
//! Collapsing them into one boolean throws away exactly the distinctions the
//! format was shaped to carry:
//!
//! - a session on an **unsynchronised clock** has perfectly good energy and a
//!   duration nobody can defend — so a per-kWh tariff bills it and a per-minute
//!   tariff must not;
//! - a session whose **identification failed** — the certificate did not check
//!   out — has good energy and nobody to bill it to.
//!
//! [`ChainFinding::disqualifies`] says which quantity each finding takes away.
//!
//! # What it enforces
//!
//! | Rule | Source | Disqualifies |
//! |---|---|---|
//! | Pagination is contiguous and ascending within one context | `[OCMF Tab. 2]` | both |
//! | All records share one pagination context | `[OCMF Tab. 2]` | both |
//! | The session opens with `TX=B` and closes with an ending marker | `[OCMF §Signing and Verification]` | both |
//! | Exactly one transaction per chain | `[OCMF Tab. 7, TX]` | both |
//! | Every record names the same signing component | `[OCMF §Relation of Serial Numbers]` | both |
//! | Readings advance in time | `[OCMF Tab. 7, TM]` | both |
//! | Only `ST=G` readings may be billed | `[OCMF Tab. 10]`, `[MessEG §33]` | both |
//! | The user assignment did not fail | `[OCMF Tab. 11]` | both |
//! | No `EF` flag this build does not understand | `[OCMF Tab. 7, EF]` | both |
//! | The billed register runs forward, on one OBIS code, in an energy unit | `[OCMF Tab. 7]` | energy |
//! | No register from a range OCMF reserved and did not define | `[OCMF Tab. 25]` | energy |
//! | Both ends of the subtraction are in the **same** unit | `[OCMF Tab. 7, RU]` | energy |
//! | No `EF` energy flag | `[OCMF Tab. 7, EF]` | energy |
//! | No `EF` time flag | `[OCMF Tab. 7, EF]` | duration |
//! | The clock is synchronised or relative | `[OCMF Tab. 19]` | duration |

use std::collections::BTreeSet;

use emob_core::{Direction, Energy, IdentificationStrength};
use rust_decimal::Decimal;

use crate::ocmf::{
    IdentificationLevel, MeterState, ObisCode, OcmfRecord, Pagination, PaginationContext,
    ReadingUnit, TimeStatus, TransactionMarker,
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

    /// More than one transaction opens in this chain.
    ///
    /// Two `TX=B` markers mean two charging processes were concatenated, and
    /// subtracting the last register value from the first spans both of them.
    MultipleTransactions {
        /// How many begin markers were found.
        count: usize,
    },

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

    /// A fault flagged the energy value as unusable (`EF` contains `E`).
    EnergyFlaggedUnusable {
        /// The pagination counter of the record.
        pagination: u64,
    },

    /// A fault flagged the time value as unusable (`EF` contains `t`).
    ///
    /// Disqualifies the duration and nothing else: the register is unaffected,
    /// and a per-kWh tariff is still perfectly billable.
    TimeFlaggedUnusable {
        /// The pagination counter of the record.
        pagination: u64,
    },

    /// The clock behind a reading is not one a duration may be billed against.
    ///
    /// `[OCMF Tab. 19]` gives four states. `S` (synchronised) and `R` (relative
    /// accounting from a calibration-law-accurate duration) qualify; `U`
    /// (unknown) and `I` (informative) do not — a time-priced session billed
    /// off an unsynchronised clock is billing a duration nobody can defend.
    ClockNotBillable {
        /// The pagination counter of the record.
        pagination: u64,
        /// What the record claimed.
        status: TimeStatus,
    },

    /// The signature component reports that the user assignment failed.
    ///
    /// `MISMATCH`, `INVALID`, `OUTDATED` and `UNKNOWN` `[OCMF Tab. 11]` are not
    /// weak assignments, they are failures: the UIDs did not match, the
    /// certificate did not check out, the trust anchor had expired, no anchor
    /// was found. The energy was still measured; there is simply nobody this
    /// chain can prove it belongs to.
    IdentificationFailed {
        /// The pagination counter of the record.
        pagination: u64,
        /// What the record reported.
        level: IdentificationLevel,
    },

    /// Two records assert different identification levels for one session.
    IdentificationChanged {
        /// The level the chain started with.
        expected: IdentificationLevel,
        /// The level that turned up.
        found: IdentificationLevel,
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
        start: ObisCode,
        /// The OBIS code at the end.
        end: ObisCode,
    },

    /// The begin and end readings are in different units, so their difference
    /// is not a quantity at all.
    ///
    /// `RU` may change between records — it is only carried forward *within*
    /// one `[OCMF Tab. 7]` — so a chain opening in kWh and closing in Wh is a
    /// thing a station can emit. Subtracting the two produces a number a
    /// thousand times wrong in whichever direction the reader guessed.
    UnitChanged {
        /// The unit at the start.
        start: ReadingUnit,
        /// The unit at the end.
        end: Option<ReadingUnit>,
    },

    /// A reading marks an exception, from which time and energy are unusable.
    ///
    /// `TX=X` is "error during charging, transaction continues, time and/or
    /// energy are no longer usable **from this reading (incl.)**"
    /// `[OCMF Tab. 7, TX]`. The transaction may well go on; the numbers may
    /// not be billed across it.
    ExceptionDuringCharging {
        /// The pagination counter of the record.
        pagination: u64,
    },

    /// Cable-loss compensation is reported on a register that does not
    /// accumulate.
    ///
    /// `CL` "can be added only when RI is indicating an accumulation register
    /// reading" `[OCMF Tab. 7, CL]`.
    LossOnNonAccumulationRegister {
        /// The pagination counter of the record.
        pagination: u64,
        /// The register it was reported against.
        register: ObisCode,
    },

    /// The cumulated loss was not reset at the start of the transaction.
    ///
    /// "CL must be reset at TX=B" `[OCMF Tab. 7, CL]`. A begin reading opening
    /// on a non-zero cumulated loss is carrying compensation from a previous
    /// session into this one.
    LossNotResetAtBegin {
        /// What the opening reading reported.
        cumulated: Decimal,
    },

    /// A reading names a register `[OCMF Tab. 25]` has reserved and not defined.
    ///
    /// `B4`–`BF` and `C4`–`C7` sit inside the range OCMF carved out to make
    /// billing-relevant data identifiable, and carry no published meaning. A
    /// station emitting one is stating a billing-relevant quantity nobody can
    /// look up — so this is not an unrecognised manufacturer register, which is
    /// still evidence and still bills, but a register whose *specification* is
    /// absent. A future revision must not be able to widen what gets billed by
    /// defining a code an older build read as "some register or other".
    ReservedRegister {
        /// The pagination counter of the record.
        pagination: u64,
        /// The register it named.
        register: ObisCode,
    },

    /// No usable energy reading pair was found.
    NoBillableEnergy,

    /// The unit is not an energy unit.
    NotAnEnergyUnit {
        /// What the reading claimed.
        unit: Option<ReadingUnit>,
    },
}

/// Which quantity a finding takes away.
///
/// The distinction is the format's, not a convenience: `EF` flags energy and
/// time separately, the clock status qualifies only the time, and a register
/// fault says nothing about how long the car was plugged in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Disqualifies {
    /// The energy may not be billed; the duration may still be.
    Energy,
    /// The duration may not be billed; the energy may still be.
    Duration,
    /// Neither may be billed.
    Both,
}

impl Disqualifies {
    /// Whether this takes the energy away.
    #[must_use]
    pub const fn energy(self) -> bool {
        matches!(self, Self::Energy | Self::Both)
    }

    /// Whether this takes the duration away.
    #[must_use]
    pub const fn duration(self) -> bool {
        matches!(self, Self::Duration | Self::Both)
    }
}

impl ChainFinding {
    /// What this finding disqualifies.
    #[must_use]
    pub const fn disqualifies(&self) -> Disqualifies {
        match self {
            // Structural faults, and faults of the meter or the user
            // assignment, reach every quantity the chain carries.
            Self::Empty
            | Self::PaginationBreak { .. }
            | Self::MixedPaginationContexts
            | Self::NoBeginMarker
            | Self::NoEndMarker
            | Self::MultipleTransactions { .. }
            | Self::SigningComponentChanged { .. }
            | Self::TimeWentBackwards { .. }
            | Self::MeterNotBillable { .. }
            | Self::IdentificationFailed { .. }
            | Self::IdentificationChanged { .. }
            // "time and/or energy are no longer usable from this reading" —
            // the specification names both, so both go.
            | Self::ExceptionDuringCharging { .. }
            // A compensation that cannot be traced is a register value nobody
            // can reproduce, and the register is where the energy comes from.
            | Self::LossOnNonAccumulationRegister { .. }
            | Self::LossNotResetAtBegin { .. }
            // A flag this build cannot interpret might disqualify either, so it
            // disqualifies both. A future OCMF revision must not be able to
            // widen what gets billed by adding a character.
            | Self::UnknownErrorFlag { .. } => Disqualifies::Both,

            // The register and its unit say nothing about the clock.
            Self::EnergyFlaggedUnusable { .. }
            | Self::RegisterRanBackwards { .. }
            | Self::ObisMismatch { .. }
            | Self::UnitChanged { .. }
            | Self::NoBillableEnergy
            // A register OCMF has reserved and not defined measures something
            // nobody can name. The clock is unaffected — the reading still has
            // a timestamp and a status.
            | Self::ReservedRegister { .. }
            | Self::NotAnEnergyUnit { .. } => Disqualifies::Energy,

            // …and the clock says nothing about the register.
            Self::TimeFlaggedUnusable { .. } | Self::ClockNotBillable { .. } => {
                Disqualifies::Duration
            }
        }
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
            Self::MultipleTransactions { count } => write!(
                f,
                "{count} transactions open in this chain: two charging processes were concatenated"
            ),
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
            Self::TimeFlaggedUnusable { pagination } => write!(
                f,
                "record {pagination} flags its time value as unusable (EF contains 't'); the energy is unaffected"
            ),
            Self::ClockNotBillable { pagination, status } => write!(
                f,
                "record {pagination} reports its clock as {status:?}, which no duration may be billed against; the energy is unaffected"
            ),
            Self::IdentificationFailed { pagination, level } => write!(
                f,
                "record {pagination} reports the user assignment as {level:?}: the energy was measured, but nobody is provably behind it"
            ),
            Self::IdentificationChanged { expected, found } => write!(
                f,
                "the identification level changed mid-session: {expected:?} then {found:?}"
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
            Self::UnitChanged { start, end } => write!(
                f,
                "the opening reading is in {start:?} and the closing one in {end:?}: their difference is not a quantity"
            ),
            Self::ExceptionDuringCharging { pagination } => write!(
                f,
                "record {pagination} marks an exception (TX=X): time and energy are unusable from that reading on"
            ),
            Self::LossOnNonAccumulationRegister {
                pagination,
                register,
            } => write!(
                f,
                "record {pagination} reports cable-loss compensation against {register}, which is not an accumulation register"
            ),
            Self::LossNotResetAtBegin { cumulated } => write!(
                f,
                "the transaction opens with {cumulated} of cumulated cable loss already on the meter; CL must be reset at TX=B"
            ),
            Self::ReservedRegister {
                pagination,
                register,
            } => write!(
                f,
                "record {pagination} reads {register}, which [OCMF Tab. 25] reserves for future use: the specification has claimed the code and not said what it measures"
            ),
            Self::NoBillableEnergy => write!(f, "no usable pair of energy readings"),
            Self::NotAnEnergyUnit { unit } => {
                write!(f, "the reading is not in an energy unit: {unit:?}")
            }
        }
    }
}

/// One transaction marker a signed reading carried, and when.
///
/// `[OCMF Tab. 7, TX]` names ten markers and most of them are structure — `B`
/// opens, `E`/`L`/`R`/`A`/`P` close, `C` is an ordinary reading. Two are
/// **facts about the session** that nothing else in the evidence states:
///
/// - `S` — "Suspended = Transaction active, but currently not charging";
/// - `T` — a tariff change.
///
/// Both are exactly the intervals money turns on. `[AFIR Art. 5(4)]` prices the
/// time a vehicle is connected and not charging per minute, and until now that
/// interval reached a CDR only from OCPP's `chargingState` — a protocol field,
/// asserted by the same party that issues the invoice. When the meter's
/// signature component states it too, the occupancy fee has evidence behind it
/// rather than an assertion, and the two can be compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SignedMarker {
    /// The pagination counter of the record that carried it.
    pub pagination: u64,
    /// When the reading was taken.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub at: time::OffsetDateTime,
    /// Which marker.
    pub marker: TransactionMarker,
}

/// What a chain of records adds up to.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainReport {
    /// Everything standing between these records and an invoice.
    pub findings: Vec<ChainFinding>,
    /// The energy the session may be billed for.
    ///
    /// `None` whenever any finding disqualifies the energy. This is the crate's
    /// central promise: **a value that does not verify does not bill**, and the
    /// type system carries it rather than a convention.
    pub billable_energy: Option<Energy>,
    /// The duration the session may be billed for.
    ///
    /// `None` whenever any finding disqualifies the duration — which is a
    /// different set of findings. A session on an unsynchronised clock has an
    /// energy an invoice may use and a duration it may not, and the two are
    /// reported separately because `[AFIR Art. 5(4)]` lets a tariff charge for
    /// both.
    pub billable_duration: Option<time::Duration>,
    /// How strongly the signed records say the user was identified.
    ///
    /// The **weakest** level any record asserted, because a chain is only as
    /// strong as its weakest claim. `None` when no record carries a user
    /// assignment at all — a fiscal reading, or a session the station did not
    /// tie to anybody.
    pub identification: Option<IdentificationStrength>,
    /// Which way the energy this chain measured was flowing, when its register
    /// says so `[OCMF Tab. 25]`.
    ///
    /// `None` for a register whose code this crate cannot classify — which is
    /// not the same as import, and a caller that needs a direction has to get
    /// it from elsewhere and know that it did.
    pub direction: Option<Direction>,
    /// The cable loss this session had compensated out of its register, when
    /// the meter reported it.
    ///
    /// `CL_end − CL_begin` `[OCMF Tab. 7, CL]`. Not subtracted from anything —
    /// the compensation is already inside `RV` — but carried, because a partner
    /// disputing the energy will ask how much of it was cable.
    ///
    /// An [`Energy`] and not a bare decimal, because `CL` "is given in the same
    /// unit as RV which is specified in RU" `[OCMF Tab. 7, CL]` — and that unit
    /// is `Wh` on ordinary German hardware. Handing a caller the raw number
    /// beside a `billable_energy` in kWh invites a figure a thousand times too
    /// large into a dispute about the very quantity it is supposed to explain.
    /// It is converted here, once, the same way the register is.
    pub compensated_loss: Option<Energy>,
    /// The OBIS register the energy was taken from.
    pub register: Option<ObisCode>,
    /// The signing component the chain belongs to.
    pub signing_component: Option<String>,
    /// Every transaction marker the readings carried, in the order they were
    /// signed `[OCMF Tab. 7, TX]`.
    ///
    /// The signed account of the session's own *shape*, beside the signed
    /// account of its energy. [`Self::suspended_intervals`] is the part that
    /// prices.
    pub timeline: Vec<SignedMarker>,
    /// When the session began.
    pub started_at: Option<time::OffsetDateTime>,
    /// When it ended.
    pub ended_at: Option<time::OffsetDateTime>,
}

impl ChainReport {
    /// Whether the session's energy may be billed.
    #[must_use]
    pub fn is_billable(&self) -> bool {
        self.billable_energy.is_some()
    }

    /// Whether a time-priced tariff may be applied to this session.
    #[must_use]
    pub fn is_billable_for_time(&self) -> bool {
        self.billable_duration.is_some()
    }

    /// The findings that take a given quantity away.
    pub fn disqualifying(&self, what: Disqualifies) -> impl Iterator<Item = &ChainFinding> {
        self.findings.iter().filter(move |f| match what {
            Disqualifies::Energy => f.disqualifies().energy(),
            Disqualifies::Duration => f.disqualifies().duration(),
            Disqualifies::Both => f.disqualifies() == Disqualifies::Both,
        })
    }

    /// The intervals the **signed records** say the transaction was active and
    /// not charging `[OCMF Tab. 7, TX]`.
    ///
    /// An `S` marker opens one and the next reading of any other kind closes
    /// it, because `TX` carries forward: a reading that does not restate the
    /// marker is still suspended, and the first that does restate it — `C`,
    /// `E`, anything — is where charging resumed or the session ended.
    ///
    /// This is what `[AFIR Art. 5(4)]`'s occupancy fee prices, stated by the
    /// component that signed the meter values rather than by the protocol the
    /// operator also controls. Empty for the ordinary station that never emits
    /// `S`, which is most of them — the marker is "can be used optionally" —
    /// and that is why it is evidence *for* a fee rather than a precondition of
    /// one.
    #[must_use]
    pub fn suspended_intervals(&self) -> Vec<(time::OffsetDateTime, time::OffsetDateTime)> {
        let mut intervals = Vec::new();
        let mut opened: Option<time::OffsetDateTime> = None;
        for entry in &self.timeline {
            match (opened, entry.marker) {
                // Already suspended and still suspended: one interval, not two.
                (Some(_), TransactionMarker::Suspended) => {}
                (None, TransactionMarker::Suspended) => opened = Some(entry.at),
                (Some(from), _) => {
                    if entry.at > from {
                        intervals.push((from, entry.at));
                    }
                    opened = None;
                }
                (None, _) => {}
            }
        }
        // A chain whose last marker is `S` has no closing reading, so the
        // session end closes it — and a chain with neither closes nothing
        // rather than inventing an end.
        if let (Some(from), Some(to)) = (opened, self.ended_at)
            && to > from
        {
            intervals.push((from, to));
        }
        intervals
    }

    /// The instants the signed records mark a tariff change at
    /// `[OCMF Tab. 7, TX]`.
    ///
    /// `[PTB-A 50.7 §3.1.7.2]` requires a tariff change to land on a settlement
    /// boundary, and `TX=T` is the station's own record of where it landed. A
    /// change the meter signed at an instant no price version starts at is a
    /// disagreement worth having before an invoice, not after.
    #[must_use]
    pub fn tariff_change_instants(&self) -> Vec<time::OffsetDateTime> {
        self.timeline
            .iter()
            .filter(|entry| entry.marker == TransactionMarker::TariffChange)
            .map(|entry| entry.at)
            .collect()
    }

    /// One line per finding, for an operator queue.
    pub fn reasons(&self) -> impl Iterator<Item = String> + '_ {
        self.findings.iter().map(ToString::to_string)
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
///     for reason in report.reasons() {
///         eprintln!("blocked: {reason}");
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
            billable_duration: None,
            identification: None,
            direction: None,
            compensated_loss: None,
            register: None,
            signing_component: None,
            timeline: Vec::new(),
            started_at: None,
            ended_at: None,
        };
    }

    check_pagination(records, &mut findings);
    let signing_component = check_signing_component(records, &mut findings);
    check_markers(records, &mut findings);
    check_time_order(records, &mut findings);
    check_reading_states(records, &mut findings);
    let identification = check_identification(records, &mut findings);

    let Energetics {
        energy,
        register,
        compensated_loss,
        started_at,
        ended_at,
    } = compute_energy(records, &mut findings);
    let direction = register.as_ref().and_then(ObisCode::direction);

    // The gates. A finding takes away the quantity it disqualifies and no
    // more — moving money on a chain that does not hold up is the one thing
    // this crate exists to prevent, and refusing to price a duration because
    // the *register* was faulty would be the same mistake pointed the other
    // way.
    let billable_energy = findings
        .iter()
        .all(|f| !f.disqualifies().energy())
        .then_some(energy)
        .flatten();
    let billable_duration = findings
        .iter()
        .all(|f| !f.disqualifies().duration())
        .then(|| match (started_at, ended_at) {
            (Some(from), Some(to)) if to >= from => Some(to - from),
            _ => None,
        })
        .flatten();

    ChainReport {
        findings,
        billable_energy,
        billable_duration,
        identification,
        direction,
        compensated_loss,
        register,
        signing_component,
        timeline: timeline_of(records),
        started_at,
        ended_at,
    }
}

/// Every transaction marker the readings carried, in signed order.
///
/// Unconditional: the timeline is what the records *say*, and a chain that does
/// not bill still has one — a dispute about an occupancy fee is exactly the case
/// where the fee was refused and somebody wants to know what the meter recorded.
fn timeline_of(records: &[OcmfRecord]) -> Vec<SignedMarker> {
    records
        .iter()
        .flat_map(|record| {
            let pagination = record.payload.pagination.number;
            record.payload.readings.iter().filter_map(move |reading| {
                reading.transaction.map(|marker| SignedMarker {
                    pagination,
                    at: reading.time.instant,
                    marker,
                })
            })
        })
        .collect()
}

/// The user assignment across the chain: every failure reported, and the
/// weakest level anybody asserted.
///
/// Only records that actually carry an assignment contribute. OCMF scopes its
/// abbreviation rules to the readings inside a record, not to the
/// identification section, and stations in the field routinely put the section
/// on the opening record only — so a record without one is silent rather than
/// contradicting.
fn check_identification(
    records: &[OcmfRecord],
    findings: &mut Vec<ChainFinding>,
) -> Option<IdentificationStrength> {
    let mut asserted: Option<IdentificationLevel> = None;
    let mut weakest: Option<IdentificationStrength> = None;

    for record in records {
        let pagination = record.payload.pagination.number;
        let Some(identification) = &record.payload.identification else {
            continue;
        };
        if !identification.assigned {
            continue;
        }
        let Some(level) = identification.level else {
            continue;
        };

        if level.is_error() {
            findings.push(ChainFinding::IdentificationFailed { pagination, level });
            continue;
        }

        match asserted {
            None => asserted = Some(level),
            Some(expected) if expected != level => {
                findings.push(ChainFinding::IdentificationChanged {
                    expected,
                    found: level,
                });
            }
            Some(_) => {}
        }

        let strength = strength_of(level);
        weakest = Some(weakest.map_or(strength, |w: IdentificationStrength| w.min(strength)));
    }

    weakest
}

/// `[OCMF Tab. 11]`'s levels, mapped onto the ordered scale.
///
/// The error levels never reach here — [`check_identification`] turns them into
/// findings first — which is why this is total rather than fallible.
const fn strength_of(level: IdentificationLevel) -> IdentificationStrength {
    match level {
        IdentificationLevel::Hearsay => IdentificationStrength::Hearsay,
        IdentificationLevel::Trusted => IdentificationStrength::Trusted,
        IdentificationLevel::Verified => IdentificationStrength::Verified,
        IdentificationLevel::Certified => IdentificationStrength::Certified,
        IdentificationLevel::Secure => IdentificationStrength::Secure,
        // `NONE`, and the error levels the caller has already reported.
        _ => IdentificationStrength::None,
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
        // `checked_add`, because a counter at `u64::MAX` would otherwise wrap
        // to zero and make the very last record of a component's life look
        // contiguous with a record numbered 0.
        if a.checked_add(1) != Some(b) {
            findings.push(ChainFinding::PaginationBreak { after: a, found: b });
        }
    }
}

/// The signing component every record must agree on `[OCMF §Relation of Serial
/// Numbers]`.
///
/// The reference is the first record that *names* one, not the first record.
/// OCMF permits a station to omit the section and anyone assembling a chain can
/// drop it, so anchoring to `records[0]` would let that one record switch the
/// comparison off and two meters sign one session unremarked.
fn check_signing_component(
    records: &[OcmfRecord],
    findings: &mut Vec<ChainFinding>,
) -> Option<String> {
    let mut named = records
        .iter()
        .filter_map(|r| r.payload.signing_component_serial());
    let first = named.next()?.to_owned();
    for serial in named {
        if serial != first {
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
    // Fiscal readings carry no transaction reference at all, so the begin/end
    // rule does not apply to them — but *every* record has to be fiscal for
    // that to hold. Judging by the first record alone let a chain whose first
    // record is fiscal and whose rest are transactional skip the marker rules
    // entirely, on top of the mixed-context finding it already earns.
    if records
        .iter()
        .all(|r| r.payload.pagination.context == PaginationContext::Fiscal)
    {
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

    // Two transactions in one chain mean two charging processes were
    // concatenated, and subtracting the last register value from the first
    // spans both of them — a number larger than either session and belonging to
    // neither. Pagination stays contiguous across the join, so nothing else
    // sees it.
    //
    // What is counted is the *transitions into* `B`, not the readings that
    // carry it. OCMF omits a reading's field when it is "identical to the
    // previous reading" `[OCMF Tab. 7]`, and `TX` is one of those fields — so a
    // record written `[{TX:"B"},{…},{TX:"E"}]` carries `B` forward onto its
    // middle reading, and counting readings would report a second transaction
    // that never happened. A genuine `[B,E,B,E]` join still counts two,
    // because the second `B` follows a marker that is not `B`.
    let begins = records
        .iter()
        .flat_map(|r| r.payload.readings.iter())
        .map(|r| {
            r.transaction
                .is_some_and(TransactionMarker::begins_transaction)
        })
        .fold((0usize, false), |(count, was_begin), is_begin| {
            (count + usize::from(is_begin && !was_begin), is_begin)
        })
        .0;
    if begins > 1 {
        findings.push(ChainFinding::MultipleTransactions { count: begins });
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
            if reading.error_flags.time_unusable {
                findings.push(ChainFinding::TimeFlaggedUnusable { pagination });
            }
            if reading
                .transaction
                .is_some_and(|m| m == TransactionMarker::Exception)
            {
                findings.push(ChainFinding::ExceptionDuringCharging { pagination });
            }
            // "CL can be added only when RI is indicating an accumulation
            // register reading" [OCMF Tab. 7, CL].
            if reading.cumulated_loss.is_some()
                && let Some(register) = &reading.obis
                && !register.is_accumulation_register()
            {
                findings.push(ChainFinding::LossOnNonAccumulationRegister {
                    pagination,
                    register: register.clone(),
                });
            }
            // A code inside OCMF's own reserved range names a billing-relevant
            // quantity the specification has not published [OCMF Tab. 25].
            if let Some(register) = &reading.obis
                && register.is_reserved_for_future_use()
            {
                findings.push(ChainFinding::ReservedRegister {
                    pagination,
                    register: register.clone(),
                });
            }
            if !reading.time.status.is_billable_for_time() {
                findings.push(ChainFinding::ClockNotBillable {
                    pagination,
                    status: reading.time.status,
                });
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

/// Everything `compute_energy` works out from the opening and closing readings.
struct Energetics {
    energy: Option<Energy>,
    register: Option<ObisCode>,
    compensated_loss: Option<Energy>,
    started_at: Option<time::OffsetDateTime>,
    ended_at: Option<time::OffsetDateTime>,
}

impl Energetics {
    /// Nothing worked out, but the session window is known.
    const fn window(
        started_at: Option<time::OffsetDateTime>,
        ended_at: Option<time::OffsetDateTime>,
    ) -> Self {
        Self {
            energy: None,
            register: None,
            compensated_loss: None,
            started_at,
            ended_at,
        }
    }
}

/// A decimal read in one of OCMF's energy units, as an [`Energy`].
///
/// The one place the conversion happens, so the register and the cable loss
/// cannot end up in different units — `Energy::from_wh` shifts the decimal point
/// rather than dividing, so a `Wh` meter's resolution survives either way.
fn energy_in(unit: ReadingUnit, value: Decimal) -> Option<Energy> {
    match unit {
        ReadingUnit::KWh => Energy::from_kwh(value).ok(),
        ReadingUnit::Wh => Energy::from_wh(value).ok(),
        // Callers filter to an energy unit first; a resistance is not one.
        ReadingUnit::MOhm | ReadingUnit::UOhm => None,
    }
}

fn compute_energy(records: &[OcmfRecord], findings: &mut Vec<ChainFinding>) -> Energetics {
    let begin = records.iter().find_map(|r| r.payload.begin_reading());
    let end = records.iter().rev().find_map(|r| r.payload.end_reading());

    let (Some(begin), Some(end)) = (begin, end) else {
        findings.push(ChainFinding::NoBillableEnergy);
        return Energetics::window(None, None);
    };

    let started_at = Some(begin.time.instant);
    let ended_at = Some(end.time.instant);

    // The registers have to be the same one, or the subtraction is between two
    // different quantities and means nothing.
    match (begin.obis.as_ref(), end.obis.as_ref()) {
        (Some(a), Some(b)) if a != b => {
            findings.push(ChainFinding::ObisMismatch {
                start: a.clone(),
                end: b.clone(),
            });
            return Energetics::window(started_at, ended_at);
        }
        _ => {}
    }

    let Some(unit) = begin.unit.filter(|u| u.is_energy()) else {
        findings.push(ChainFinding::NotAnEnergyUnit { unit: begin.unit });
        return Energetics::window(started_at, ended_at);
    };

    // `RU` is carried forward only *within* a record `[OCMF Tab. 7]`, so a
    // chain that opens in kWh and closes in Wh is something a station can
    // actually emit — and subtracting the two is a number a thousand times
    // wrong. The register check above compares the OBIS codes; this compares
    // what they were counted in, which is a separate claim.
    if end.unit != Some(unit) {
        findings.push(ChainFinding::UnitChanged {
            start: unit,
            end: end.unit,
        });
        return Energetics::window(started_at, ended_at);
    }

    // "CL must be reset at TX=B" [OCMF Tab. 7, CL]. A transaction opening on a
    // non-zero cumulated loss is carrying compensation from a previous session
    // into this one.
    if let Some(opening_loss) = begin.cumulated_loss
        && !opening_loss.is_zero()
    {
        findings.push(ChainFinding::LossNotResetAtBegin {
            cumulated: opening_loss,
        });
    }
    // Not subtracted from anything: the compensation is already inside `RV`.
    // Carried, because a partner disputing the energy will ask how much of it
    // was cable — and carried in the same unit as the energy it explains, which
    // means converting it, because `CL` is in `RU` and `RU` is `Wh` on
    // ordinary German hardware `[OCMF Tab. 7, CL]`.
    //
    // `CL_end − CL_begin`, and the subtraction is not ceremony: it is what the
    // quantity *is*, and a chain whose opening reading carried a non-zero `CL`
    // has already earned `LossNotResetAtBegin` above rather than silently
    // reporting a previous session's cable as this one's.
    let compensated_loss = end.cumulated_loss.map(|end_loss| {
        let delta = end_loss - begin.cumulated_loss.unwrap_or(Decimal::ZERO);
        energy_in(unit, delta).unwrap_or(Energy::ZERO)
    });

    let (Some(start_value), Some(end_value)) = (begin.value, end.value) else {
        findings.push(ChainFinding::NoBillableEnergy);
        return Energetics::window(started_at, ended_at);
    };

    if end_value < start_value {
        findings.push(ChainFinding::RegisterRanBackwards {
            start: start_value,
            end: end_value,
        });
        return Energetics::window(started_at, ended_at);
    }

    // The subtraction OCMF prescribes, done exactly, with the meter's own
    // scale preserved on both sides.
    let energy = energy_in(unit, end_value - start_value);

    Energetics {
        energy,
        register: begin.obis.clone(),
        compensated_loss,
        started_at,
        ended_at,
    }
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
        assert_eq!(
            report.register.as_ref().map(ObisCode::as_str),
            Some("01-00:B2.08.00*FF")
        );
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
    fn the_signed_records_state_when_the_vehicle_stopped_charging() {
        // `[OCMF Tab. 7, TX]`: "S – Suspended = Transaction active, but
        // currently not charging". That is precisely the interval
        // `[AFIR Art. 5(4)]` prices per minute, and until it is read the
        // occupancy fee rests on OCPP alone — a field asserted by the party
        // that issues the invoice.
        let session = vec![
            record(1, "B", "100.000", "G", 0),
            record(2, "S", "110.000", "G", 20),
            record(3, "C", "110.000", "G", 40),
            record(4, "E", "118.000", "G", 55),
        ];
        let report = validate(&session);
        assert!(report.findings.is_empty(), "{:?}", report.findings);

        assert_eq!(report.timeline.len(), 4);
        assert_eq!(report.timeline[1].marker, TransactionMarker::Suspended);
        assert_eq!(report.timeline[1].pagination, 2);

        let suspended = report.suspended_intervals();
        assert_eq!(suspended.len(), 1);
        assert_eq!(
            (
                (suspended[0].1 - suspended[0].0).whole_minutes(),
                suspended[0].0.minute()
            ),
            (20, 20),
            "10:20 to 10:40, from the marker to the reading that resumed"
        );

        // The energy is unaffected: a suspension is a fact about the session,
        // not a fault.
        assert_eq!(report.billable_energy.unwrap().to_string(), "18.000 kWh");
    }

    #[test]
    fn a_suspension_that_is_never_resumed_closes_at_the_session_end() {
        // The ordinary shape: the car finishes, the driver comes back later,
        // and the last thing the meter signs is the end of the transaction.
        let session = vec![
            record(1, "B", "100.000", "G", 0),
            record(2, "S", "110.000", "G", 20),
            record(3, "E", "110.000", "G", 45),
        ];
        let report = validate(&session);
        let suspended = report.suspended_intervals();
        assert_eq!(suspended.len(), 1);
        assert_eq!((suspended[0].1 - suspended[0].0).whole_minutes(), 25);

        // Two consecutive `S` readings are one interval, not two — `TX` carries
        // forward and a station may restate it.
        let session = vec![
            record(1, "B", "100.000", "G", 0),
            record(2, "S", "110.000", "G", 20),
            record(3, "S", "110.000", "G", 30),
            record(4, "E", "110.000", "G", 45),
        ];
        let report = validate(&session);
        let suspended = report.suspended_intervals();
        assert_eq!(suspended.len(), 1, "{suspended:?}");
        assert_eq!((suspended[0].1 - suspended[0].0).whole_minutes(), 25);

        // …and a station that never emits `S` — most of them — reports none,
        // which is why this is evidence for a fee rather than a precondition.
        let report = validate(&good_session());
        assert!(report.suspended_intervals().is_empty());
        assert!(report.tariff_change_instants().is_empty());
    }

    #[test]
    fn a_signed_tariff_change_is_an_instant_a_price_version_has_to_start_at() {
        // `[PTB-A 50.7 §3.1.7.2]` puts a tariff change on a settlement
        // boundary, and `TX=T` is the station's own record of where it landed.
        let session = vec![
            record(1, "B", "100.000", "G", 0),
            record(2, "T", "110.000", "G", 15),
            record(3, "E", "118.000", "G", 30),
        ];
        let report = validate(&session);
        let changes = report.tariff_change_instants();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].minute(), 15);
        assert!(
            emob_core::QuarterHour::is_boundary(changes[0]),
            "and this one lands where the metrology document requires"
        );
    }

    #[test]
    fn a_register_ocmf_reserved_and_never_defined_blocks_the_energy() {
        // The same argument as the unknown error flag, one field over: a future
        // OCMF revision must not be able to widen what gets billed by defining
        // a code an older build read as "some register or other".
        let session = vec![
            ocmf::parse(r#"OCMF|{"PG":"T1","MS":"BQ1","RD":[{"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":10.000,"RI":"01-00:B4.08.00*FF","RU":"kWh","EF":"","ST":"G"}]}|{"SD":"00"}"#).unwrap(),
            ocmf::parse(r#"OCMF|{"PG":"T2","MS":"BQ1","RD":[{"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":20.000,"RI":"01-00:B4.08.00*FF","RU":"kWh","EF":"","ST":"G"}]}|{"SD":"00"}"#).unwrap(),
        ];
        let report = validate(&session);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, ChainFinding::ReservedRegister { .. })),
            "{:?}",
            report.findings
        );
        assert!(!report.is_billable());
        assert!(
            report.is_billable_for_time(),
            "the clock is unaffected: the reading still has a timestamp and a status"
        );

        // A *manufacturer* register this crate has never seen is a different
        // thing, and still bills — it is evidence, just evidence that states no
        // direction.
        let manufacturer = vec![
            ocmf::parse(r#"OCMF|{"PG":"T1","MS":"BQ1","RD":[{"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":10.000,"RI":"01-00:99.08.00*FF","RU":"kWh","EF":"","ST":"G"}]}|{"SD":"00"}"#).unwrap(),
            ocmf::parse(r#"OCMF|{"PG":"T2","MS":"BQ1","RD":[{"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":20.000,"RI":"01-00:99.08.00*FF","RU":"kWh","EF":"","ST":"G"}]}|{"SD":"00"}"#).unwrap(),
        ];
        let report = validate(&manufacturer);
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert_eq!(report.billable_energy.unwrap().to_string(), "10.000 kWh");
        assert_eq!(report.direction, None, "and it states no direction");
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
    fn a_swapped_component_is_caught_even_when_the_first_record_names_none() {
        // OCMF lets a station omit `MS`, and anyone assembling a chain can drop
        // it — so an anchor on `records[0]` alone is one that record can switch
        // off. Two meters sign the session below.
        let anonymous = ocmf::parse(
            r#"OCMF|{"PG":"T1","RD":[{"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":2935.600,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}]}|{"SD":"00"}"#,
        )
        .unwrap();
        let other = ocmf::parse(
            r#"OCMF|{"PG":"T3","MS":"OTHER","RD":[{"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":2965.100,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}]}|{"SD":"00"}"#,
        )
        .unwrap();

        let report = validate(&[anonymous, record(2, "C", "2950.000", "G", 10), other]);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, ChainFinding::SigningComponentChanged { .. })),
            "two meters signed one session: {:?}",
            report.findings
        );
        assert!(!report.is_billable());
        assert_eq!(
            report.signing_component.as_deref(),
            Some("BQ1"),
            "the reference is the first record that names one"
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
    fn readings_in_different_units_do_not_subtract() {
        // `RU` carries forward only inside one record, so a chain that opens in
        // kWh and closes in Wh is something a station can emit — and the naive
        // subtraction is a number a thousand times wrong.
        let end = ocmf::parse(
            r#"OCMF|{"PG":"T2","MS":"BQ1","RD":[{"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":2965100,"RI":"01-00:B2.08.00*FF","RU":"Wh","EF":"","ST":"G"}]}|{"SD":"00"}"#,
        )
        .unwrap();
        let report = validate(&[record(1, "B", "2935.600", "G", 0), end]);

        assert!(
            report.findings.contains(&ChainFinding::UnitChanged {
                start: ReadingUnit::KWh,
                end: Some(ReadingUnit::Wh),
            }),
            "{:?}",
            report.findings
        );
        assert!(!report.is_billable());
        assert!(
            report.is_billable_for_time(),
            "the clock is untouched by a unit muddle"
        );
    }

    #[test]
    fn a_carried_forward_begin_marker_is_not_a_second_transaction() {
        // OCMF omits a reading's field when it is identical to the previous
        // one, and `TX` is such a field — so the middle reading here inherits
        // `B`. Counting readings rather than transitions reported a second
        // charging process that never happened, and refused to bill a perfectly
        // ordinary session.
        let raw = r#"OCMF|{"PG":"T1","MS":"BQ1","RD":[
            {"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":2935.600,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"},
            {"TM":"2026-01-02T10:10:00,000+0100 S","RV":2950.000,"EF":"","ST":"G"},
            {"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":2965.100,"EF":"","ST":"G"}
        ]}|{"SD":"00"}"#;
        let report = validate(&[ocmf::parse(raw).unwrap()]);

        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert_eq!(report.billable_energy.unwrap().to_string(), "29.500 kWh");
    }

    #[test]
    fn a_fiscal_first_record_does_not_switch_the_marker_rules_off() {
        // Judging the context by the first record alone let a chain skip the
        // begin/end rules entirely by putting one fiscal record in front.
        let fiscal = ocmf::parse(
            r#"OCMF|{"PG":"F1","MS":"BQ1","RD":[{"TM":"2026-01-02T10:00:00,000+0100 S","RV":100.0,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}]}|{"SD":"00"}"#,
        )
        .unwrap();
        let report = validate(&[fiscal, record(2, "C", "2950.000", "G", 10)]);

        assert!(
            report
                .findings
                .contains(&ChainFinding::MixedPaginationContexts)
        );
        assert!(
            report.findings.contains(&ChainFinding::NoBeginMarker)
                && report.findings.contains(&ChainFinding::NoEndMarker),
            "the marker rules still apply: {:?}",
            report.findings
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

    /// A record with an identification section and a chosen clock status.
    fn record_with(
        pg: u64,
        tx: &str,
        value: &str,
        minute: u8,
        ident: &str,
        clock: &str,
    ) -> OcmfRecord {
        let raw = format!(
            r#"OCMF|{{"PG":"T{pg}","MS":"BQ1",{ident}"RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 {clock}","TX":"{tx}","RV":{value},"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}}]}}|{{"SD":"00"}}"#
        );
        ocmf::parse(&raw).unwrap()
    }

    #[test]
    fn an_unsynchronised_clock_takes_the_duration_and_leaves_the_energy() {
        // The distinction the format carries and almost no implementation
        // reads: `[OCMF Tab. 19]` says `U` is an unsynchronised clock. The
        // register is untouched, so a per-kWh tariff bills — and a per-minute
        // one must not.
        let session = vec![
            record_with(1, "B", "2935.600", 0, "", "U"),
            record_with(2, "E", "2965.100", 20, "", "U"),
        ];
        let report = validate(&session);

        assert_eq!(
            report.billable_energy.unwrap().to_string(),
            "29.500 kWh",
            "the energy is perfectly good"
        );
        assert!(
            !report.is_billable_for_time(),
            "and the duration is not: nobody can defend it"
        );
        assert!(
            report
                .findings
                .iter()
                .all(|f| matches!(f, ChainFinding::ClockNotBillable { .. }))
        );
        assert_eq!(report.disqualifying(Disqualifies::Duration).count(), 2);
        assert_eq!(report.disqualifying(Disqualifies::Energy).count(), 0);
    }

    #[test]
    fn a_synchronised_session_bills_both_quantities() {
        let report = validate(&good_session());
        assert_eq!(report.billable_energy.unwrap().to_string(), "29.500 kWh");
        assert_eq!(
            report.billable_duration,
            Some(time::Duration::minutes(20)),
            "10:00 to 10:20"
        );
        assert!(report.is_billable_for_time());
    }

    #[test]
    fn a_relative_clock_is_good_enough_for_a_duration() {
        // `R` is relative accounting from a calibration-law-accurate duration
        // on top of an informative start time — which is exactly what a
        // duration needs, and not what an absolute timestamp needs.
        let session = vec![
            record_with(1, "B", "2935.600", 0, "", "R"),
            record_with(2, "E", "2965.100", 20, "", "R"),
        ];
        assert!(validate(&session).is_billable_for_time());
    }

    #[test]
    fn a_time_error_flag_takes_the_duration_and_nothing_else() {
        let end = ocmf::parse(
            r#"OCMF|{"PG":"T2","MS":"BQ1","RD":[{"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":2965.100,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"t","ST":"G"}]}|{"SD":"00"}"#,
        )
        .unwrap();
        let report = validate(&[record(1, "B", "2935.600", "G", 0), end]);

        assert!(
            report
                .findings
                .contains(&ChainFinding::TimeFlaggedUnusable { pagination: 2 })
        );
        assert!(report.is_billable(), "the register is unaffected");
        assert!(!report.is_billable_for_time());
    }

    #[test]
    fn an_energy_error_flag_takes_the_energy_and_leaves_the_duration() {
        // The mirror image, and the reason the two gates are separate.
        let end = ocmf::parse(
            r#"OCMF|{"PG":"T2","MS":"BQ1","RD":[{"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":2965.100,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"E","ST":"G"}]}|{"SD":"00"}"#,
        )
        .unwrap();
        let report = validate(&[record(1, "B", "2935.600", "G", 0), end]);

        assert!(!report.is_billable());
        assert!(
            report.is_billable_for_time(),
            "the car was still plugged in for twenty minutes"
        );
    }

    #[test]
    fn a_failed_user_assignment_blocks_everything() {
        // `INVALID` is not a weak assignment. The certificate did not check
        // out, so the energy was measured and there is nobody to bill it to.
        let session = vec![
            record_with(
                1,
                "B",
                "2935.600",
                0,
                r#""IS":true,"IL":"INVALID","IT":"ISO15118","#,
                "S",
            ),
            record_with(2, "E", "2965.100", 20, "", "S"),
        ];
        let report = validate(&session);

        assert!(report.findings.iter().any(|f| matches!(
            f,
            ChainFinding::IdentificationFailed {
                pagination: 1,
                level: IdentificationLevel::Invalid
            }
        )));
        assert!(!report.is_billable());
        assert!(!report.is_billable_for_time());
        assert!(
            report
                .reasons()
                .any(|r| r.contains("nobody is provably behind it")),
            "{:?}",
            report.reasons().collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_identification_strength_is_the_weakest_any_record_asserted() {
        // A chain is only as strong as its weakest claim, and over-reporting is
        // what the CDR cross-check exists to catch.
        let session = vec![
            record_with(
                1,
                "B",
                "2935.600",
                0,
                r#""IS":true,"IL":"SECURE","IT":"ISO15118","#,
                "S",
            ),
            record_with(
                2,
                "E",
                "2965.100",
                20,
                r#""IS":true,"IL":"HEARSAY","IT":"ISO14443","#,
                "S",
            ),
        ];
        let report = validate(&session);

        assert_eq!(report.identification, Some(IdentificationStrength::Hearsay));
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, ChainFinding::IdentificationChanged { .. })),
            "…and the disagreement is reported rather than silently resolved"
        );
    }

    #[test]
    fn a_record_without_an_assignment_is_silent_rather_than_contradicting() {
        // Stations routinely put the identification section on the opening
        // record only. That is not a change of identity.
        let session = vec![
            record_with(
                1,
                "B",
                "2935.600",
                0,
                r#""IS":true,"IL":"TRUSTED","IT":"CENTRAL","#,
                "S",
            ),
            record_with(2, "E", "2965.100", 20, "", "S"),
        ];
        let report = validate(&session);

        assert_eq!(report.identification, Some(IdentificationStrength::Trusted));
        assert!(report.findings.is_empty(), "{:?}", report.findings);
    }

    #[test]
    fn an_unassigned_session_reports_no_strength_rather_than_a_weak_one() {
        let report = validate(&good_session());
        assert_eq!(report.identification, None);
        assert!(
            report.is_billable(),
            "which does not stop the energy billing"
        );
    }

    #[test]
    fn two_transactions_in_one_chain_are_caught() {
        // Pagination stays contiguous across the join, so nothing else sees it
        // — and the subtraction would span both sessions.
        let session = vec![
            record(1, "B", "100.000", "G", 0),
            record(2, "E", "110.000", "G", 10),
            record(3, "B", "200.000", "G", 20),
            record(4, "E", "215.000", "G", 30),
        ];
        let report = validate(&session);

        assert!(
            report
                .findings
                .contains(&ChainFinding::MultipleTransactions { count: 2 })
        );
        assert!(!report.is_billable(), "115 kWh belongs to neither session");
    }

    /// A record on a chosen OBIS register, optionally with a cumulated loss.
    fn record_on(pg: u64, tx: &str, value: &str, minute: u8, obis: &str, cl: &str) -> OcmfRecord {
        let raw = format!(
            r#"OCMF|{{"PG":"T{pg}","MS":"BQ1","RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{value},{cl}"RI":"{obis}","RU":"kWh","EF":"","ST":"G"}}]}}|{{"SD":"00"}}"#
        );
        ocmf::parse(&raw).unwrap()
    }

    #[test]
    fn the_register_states_which_way_the_energy_went() {
        // `[OCMF Tab. 25]`: `B2` is transaction import, `C2` transaction export.
        // A workspace whose central claim is that the two never net cannot take
        // the direction from anywhere but the signed register.
        let import = validate(&good_session());
        assert_eq!(import.direction, Some(Direction::Import));

        let export = vec![
            record_on(1, "B", "0.000", 0, "01-00:C2.08.00*FF", ""),
            record_on(2, "E", "7.500", 20, "01-00:C2.08.00*FF", ""),
        ];
        let report = validate(&export);
        assert_eq!(report.direction, Some(Direction::Export));
        assert_eq!(report.billable_energy.unwrap().to_string(), "7.500 kWh");
        assert!(report.findings.is_empty(), "{:?}", report.findings);
    }

    #[test]
    fn a_register_this_crate_cannot_classify_states_no_direction() {
        // Not import by default. A caller that needs a direction has to get it
        // from elsewhere and know that it did.
        let unknown = vec![
            record_on(1, "B", "0.000", 0, "01-00:99.08.00*FF", ""),
            record_on(2, "E", "7.500", 20, "01-00:99.08.00*FF", ""),
        ];
        let report = validate(&unknown);
        assert_eq!(report.direction, None);
        assert!(
            report.is_billable(),
            "which does not stop the energy billing"
        );
    }

    #[test]
    fn an_exception_marker_makes_both_quantities_unusable() {
        // "X – Exception = Error during charging, transaction continues, time
        // and/or energy are no longer usable from this reading (incl.)"
        // [OCMF Tab. 7, TX]. The transaction goes on; the numbers may not be
        // billed across it.
        let session = vec![
            record(1, "B", "2935.600", "G", 0),
            record(2, "X", "2950.000", "G", 10),
            record(3, "E", "2965.100", "G", 20),
        ];
        let report = validate(&session);

        assert!(
            report
                .findings
                .contains(&ChainFinding::ExceptionDuringCharging { pagination: 2 })
        );
        assert!(!report.is_billable());
        assert!(!report.is_billable_for_time());
        assert!(report.reasons().any(|r| r.contains("TX=X")));
    }

    #[test]
    fn cable_loss_must_be_reset_at_the_start_of_a_transaction() {
        // "CL must be reset at TX=B" [OCMF Tab. 7, CL]. A transaction opening
        // on a non-zero cumulated loss is carrying compensation from a previous
        // session into this one.
        let carried_over = vec![
            record_on(1, "B", "2935.600", 0, "01-00:B2.08.00*FF", r#""CL":1.5,"#),
            record_on(2, "E", "2965.100", 20, "01-00:B2.08.00*FF", r#""CL":2.0,"#),
        ];
        let report = validate(&carried_over);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, ChainFinding::LossNotResetAtBegin { .. }))
        );
        assert!(!report.is_billable());
    }

    #[test]
    fn the_compensated_cable_loss_is_carried_rather_than_subtracted() {
        // The compensation is already inside `RV`. It is reported because a
        // partner disputing the energy will ask how much of it was cable.
        let compensated = vec![
            record_on(1, "B", "2935.600", 0, "01-00:B3.08.00*FF", r#""CL":0,"#),
            record_on(2, "E", "2965.100", 20, "01-00:B3.08.00*FF", r#""CL":0.42,"#),
        ];
        let report = validate(&compensated);

        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert_eq!(
            report.billable_energy.unwrap().to_string(),
            "29.500 kWh",
            "the register is the register; the loss is not deducted twice"
        );
        assert_eq!(
            report.compensated_loss.unwrap().to_string(),
            "0.42 kWh",
            "in the unit the register was in, because CL is given in RU"
        );
    }

    #[test]
    fn the_cable_loss_is_converted_from_the_registers_own_unit() {
        // `CL` "is given in the same unit as RV which is specified in RU"
        // [OCMF Tab. 7, CL] — and `RU` is `Wh` on ordinary German hardware.
        // Reported raw beside a kWh energy, a 420 Wh cable loss reads as
        // 420 kWh: a figure a thousand times larger than the session it is
        // supposed to explain, in the middle of a dispute about that session.
        let raw = r#"OCMF|{"PG":"T1","MS":"BQ1","RD":[
            {"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":1000,"RI":"01-00:B2.08.00*FF","RU":"Wh","CL":0,"EF":"","ST":"G"},
            {"TM":"2026-01-02T10:20:00,000+0100 S","TX":"E","RV":30500,"CL":420,"EF":"","ST":"G"}
        ]}|{"SD":"00"}"#;
        let report = validate(&[ocmf::parse(raw).unwrap()]);

        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert_eq!(report.billable_energy.unwrap().to_string(), "29.500 kWh");
        assert_eq!(
            report.compensated_loss.unwrap().to_string(),
            "0.420 kWh",
            "420 Wh, in the unit the energy beside it is quoted in"
        );
    }

    #[test]
    fn cable_loss_on_a_register_that_does_not_accumulate_is_refused() {
        // "CL can be added only when RI is indicating an accumulation register
        // reading" [OCMF Tab. 7, CL] — `.08.` is the time integral.
        let wrong_register = vec![
            record_on(1, "B", "0.000", 0, "01-00:B2.07.00*FF", r#""CL":0,"#),
            record_on(2, "E", "1.000", 20, "01-00:B2.07.00*FF", r#""CL":0.1,"#),
        ];
        let report = validate(&wrong_register);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, ChainFinding::LossOnNonAccumulationRegister { .. }))
        );
    }

    #[test]
    fn every_finding_disqualifies_something_it_can_name() {
        // The property that keeps the two gates honest: a finding that
        // disqualified nothing would be a finding that changes no answer.
        for finding in [
            ChainFinding::Empty,
            ChainFinding::PaginationBreak { after: 1, found: 3 },
            ChainFinding::MixedPaginationContexts,
            ChainFinding::NoBeginMarker,
            ChainFinding::NoEndMarker,
            ChainFinding::MultipleTransactions { count: 2 },
            ChainFinding::EnergyFlaggedUnusable { pagination: 1 },
            ChainFinding::TimeFlaggedUnusable { pagination: 1 },
            ChainFinding::ClockNotBillable {
                pagination: 1,
                status: crate::ocmf::TimeStatus::Unknown,
            },
            ChainFinding::IdentificationFailed {
                pagination: 1,
                level: IdentificationLevel::Mismatch,
            },
            ChainFinding::NoBillableEnergy,
            ChainFinding::ExceptionDuringCharging { pagination: 1 },
            ChainFinding::LossNotResetAtBegin {
                cumulated: Decimal::ONE,
            },
            ChainFinding::LossOnNonAccumulationRegister {
                pagination: 1,
                register: ObisCode::new("01-00:B2.07.00*FF"),
            },
        ] {
            let d = finding.disqualifies();
            assert!(
                d.energy() || d.duration(),
                "{finding:?} disqualifies nothing"
            );
        }
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
            ChainFinding::MultipleTransactions { count: 2 },
            ChainFinding::TimeFlaggedUnusable { pagination: 1 },
            ChainFinding::ClockNotBillable {
                pagination: 1,
                status: crate::ocmf::TimeStatus::Unknown,
            },
            ChainFinding::IdentificationFailed {
                pagination: 1,
                level: IdentificationLevel::Outdated,
            },
            ChainFinding::IdentificationChanged {
                expected: IdentificationLevel::Secure,
                found: IdentificationLevel::Hearsay,
            },
            ChainFinding::ExceptionDuringCharging { pagination: 2 },
            ChainFinding::LossNotResetAtBegin {
                cumulated: Decimal::ONE,
            },
            ChainFinding::LossOnNonAccumulationRegister {
                pagination: 2,
                register: ObisCode::new("01-00:B2.07.00*FF"),
            },
        ] {
            let text = finding.to_string();
            assert!(text.len() > 10, "{finding:?} renders as {text:?}");
        }
    }
}
