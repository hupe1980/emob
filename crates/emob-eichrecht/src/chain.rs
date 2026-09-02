//! What a set of signed records proves *together*, and which quantity each
//! failure takes away.
//!
//! # The division of labour
//!
//! [`ocmf::session`] answers "is this a whole, unaltered sequence of records
//! from one meter" — the check component
//! `[OCMF §Signing and Verification Process]` demands. It will not decide money,
//! and says so: *"whether a session may be invoiced depends on tariffs, on a key
//! registry binding each record to this charge point, and on law — none of which
//! is in scope."*
//!
//! That sentence is this module. A format crate can say a record is missing;
//! only law can say what a missing record costs you, and the answer is not one
//! boolean.
//!
//! # Three quantities, not one
//!
//! OCMF states them separately. A record carries `EF` flags for energy (`E`) and
//! time (`t`) apart `[OCMF Tab. 7]`, its clock's trustworthiness separately
//! `[OCMF Tab. 19]`, and the user assignment separately from both
//! `[OCMF Tab. 11]`. Collapsing them loses exactly what the format was shaped to
//! carry: a session on an unsynchronised clock has good energy and a duration
//! nobody can defend, and one whose identification failed has good energy and
//! nobody to bill it to.
//!
//! [`ChainFinding::disqualifies`] is that mapping, and it is **total** over
//! `ocmf::session::Finding` with a fallback of `Both` — a fault a later release
//! adds must not widen what this build bills by being unrecognised.
//!
//! # What this adds to the sequence rules
//!
//! Five checks that are about billing rather than about the format:
//!
//! | Rule | Source | Disqualifies |
//! |---|---|---|
//! | The billed register is not one `[OCMF Tab. 25]` reserved and never defined | `[OCMF Tab. 25]` | energy |
//! | The reading is in an energy unit at all | `[OCMF Tab. 7, RU]` | energy |
//! | Cable-loss compensation is reported against an accumulation register | `[OCMF Tab. 7, CL]` | both |
//! | …and is reset at `TX=B`, so the session's own loss is `CL_end` | `[OCMF Tab. 7, CL]` | both |
//! | The identification level does not change mid-session | `[OCMF Tab. 11]` | both |
//! | An `EF` character this build does not know | `[OCMF Tab. 7, EF]` | both |

use emob_core::{Direction, Energy, IdentificationStrength};
use ocmf::obis::Register;
use ocmf::session::{self, Finding, SessionReport};
use ocmf::{IdentificationLevel, MeterState, ObisCode, Record, TimeStatus, TransactionMarker};
use rust_decimal::Decimal;

/// Which quantity a finding takes away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// Something that stands between a set of records and an invoice.
///
/// Two sources, deliberately visible in the type. [`Self::Sequence`] wraps what
/// `ocmf::session` found about the records *as records*; every other variant is
/// a rule about what may be **billed**, which is this crate's own question.
///
/// Keeping them apart rather than flattening them into one list means a reader
/// can tell a format fault from a billing rule without reading the enum's
/// documentation, and means an upgrade of `ocmf` cannot quietly change what
/// this crate refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChainFinding {
    /// A rule about the sequence itself `[OCMF §Signing and Verification
    /// Process]`, from [`ocmf::session`].
    Sequence(Finding),

    /// The billed register is one `[OCMF Tab. 25]` reserved and never defined.
    ///
    /// The specification has claimed the code and not said what it measures, so
    /// a value read from it is a number with no stated meaning. Disqualifies the
    /// energy and nothing else: the reading still has a timestamp and a status.
    ReservedRegister {
        /// The register, as written.
        register: String,
    },

    /// The reading is not in an energy unit `[OCMF Tab. 7, RU]`.
    ///
    /// `mOhm` and `uOhm` are lawful `RU` values — a meter reporting cable
    /// resistance — and a difference of two of them is not a quantity anybody
    /// may bill for electricity.
    NotAnEnergyUnit {
        /// The unit, as written.
        unit: String,
    },

    /// Cable-loss compensation was reported against a register that does not
    /// accumulate `[OCMF Tab. 7, CL]`.
    ///
    /// `CL` "is given in the same unit as RV" and states how much of the
    /// register is cable rather than vehicle. Reported against something that is
    /// not an accumulating energy register, it is a compensation nobody can
    /// trace to a value — and the register is where the energy comes from.
    LossOnNonAccumulationRegister {
        /// The register it was reported against.
        register: String,
    },

    /// The transaction opens with cumulated cable loss already on the meter.
    ///
    /// `CL` accumulates across the transaction, so the session's own
    /// compensation is `CL_end` and not `CL_end − CL_begin` — unless the meter
    /// failed to reset it at `TX=B`, in which case neither reading means what a
    /// reader will take it to mean.
    LossNotResetAtBegin {
        /// What the opening record already carried.
        cumulated: String,
    },

    /// The identification level changed mid-session `[OCMF Tab. 11]`.
    ///
    /// A session identified one way at its start and another at its end is one
    /// whose records cannot all be about the same authorisation.
    IdentificationChanged {
        /// The level the chain started with, as written.
        expected: String,
        /// The level that turned up.
        found: String,
    },

    /// A record carries an `EF` character this build does not know
    /// `[OCMF Tab. 7, EF]`.
    ///
    /// Disqualifies both, because a flag this build cannot interpret might
    /// disqualify either. A future revision of the format must not be able to
    /// widen what gets billed by adding a character.
    UnknownErrorFlag {
        /// The flags, as written.
        flags: String,
    },

    /// No usable pair of energy readings on one register.
    NoBillableEnergy,
}

impl ChainFinding {
    /// What this finding takes away.
    ///
    /// **Total over both sources.** Every `ocmf::session::Finding` has an answer
    /// here, and the `#[non_exhaustive]` catch-all is `Both` rather than
    /// "nothing" — a fault this build has not learned to classify must not
    /// widen what gets billed.
    #[must_use]
    pub fn disqualifies(&self) -> Disqualifies {
        match self {
            Self::Sequence(finding) => sequence_disqualifies(finding),

            // The register and its unit say nothing about the clock.
            Self::ReservedRegister { .. }
            | Self::NotAnEnergyUnit { .. }
            | Self::NoBillableEnergy => Disqualifies::Energy,

            // A compensation that cannot be traced is a register value nobody
            // can reproduce, an identification that changed is a session nobody
            // can attribute, and a flag nobody can read might mean either.
            Self::LossOnNonAccumulationRegister { .. }
            | Self::LossNotResetAtBegin { .. }
            | Self::IdentificationChanged { .. }
            | Self::UnknownErrorFlag { .. } => Disqualifies::Both,
        }
    }
}

/// What a sequence finding takes away.
///
/// The one place in this workspace where `ocmf`'s vocabulary becomes a billing
/// decision, and it is a `match` rather than a default so that adding a variant
/// upstream is a compile error here rather than a silent `Both`.
fn sequence_disqualifies(finding: &Finding) -> Disqualifies {
    match finding {
        // `EF` names its quantities separately, and the whole point of this
        // module is that they are separate. A record flagging `E` has a good
        // clock; one flagging `t` has a good register.
        Finding::ErrorFlagged { flags, .. } => {
            match (flags.contains('E'), flags.contains('t')) {
                (true, false) => Disqualifies::Energy,
                (false, true) => Disqualifies::Duration,
                // Both flags, or a flag string this mapping cannot read — which
                // `ChainFinding::UnknownErrorFlag` reports separately and which
                // must not be allowed to fall through to a narrower answer.
                _ => Disqualifies::Both,
            }
        }

        // The clock, and nothing but the clock.
        Finding::ClockNotSynchronised { .. } => Disqualifies::Duration,

        // The register, and nothing but the register.
        Finding::MeterWentBackwards { .. } | Finding::RegisterEndWithoutBegin { .. } => {
            Disqualifies::Energy
        }

        // Everything else takes everything away: a sequence with a hole in it,
        // a meter that was not working, an assignment that failed, an exception
        // mid-charge — and, because `ocmf::session::Finding` is
        // `#[non_exhaustive]`, any fault a later release of that crate adds.
        //
        // The fallback direction is the whole point. A new upstream check must
        // not be able to widen what this build bills by being unrecognised, so
        // "not classified" and "structural" share an answer rather than a
        // `match` arm apiece — and the three narrow answers above are the
        // exhaustive list of what is *not* fatal.
        _ => Disqualifies::Both,
    }
}

impl core::fmt::Display for ChainFinding {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Sequence(finding) => write!(f, "{finding}"),
            Self::ReservedRegister { register } => write!(
                f,
                "the billed register is {register}, which [OCMF Tab. 25] reserves for future use: the specification has claimed the code and not said what it measures"
            ),
            Self::NotAnEnergyUnit { unit } => {
                write!(f, "the reading is not in an energy unit: {unit}")
            }
            Self::LossOnNonAccumulationRegister { register } => write!(
                f,
                "cable-loss compensation is reported against {register}, which is not an accumulation register"
            ),
            Self::LossNotResetAtBegin { cumulated } => write!(
                f,
                "the transaction opens with {cumulated} of cumulated cable loss already on the meter; CL must be reset at TX=B"
            ),
            Self::IdentificationChanged { expected, found } => write!(
                f,
                "the identification level changed mid-session: {expected} then {found}"
            ),
            Self::UnknownErrorFlag { flags } => write!(
                f,
                "a record carries error flags this build does not know: {flags:?}"
            ),
            Self::NoBillableEnergy => write!(f, "no usable pair of energy readings"),
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
    ///
    /// Travels as the **letter** `[OCMF Tab. 7]` defines rather than as a
    /// variant name: the letter is the specification's own token and does not
    /// move when a Rust enum is renamed.
    #[cfg_attr(feature = "serde", serde(with = "marker_letter"))]
    pub marker: TransactionMarker,
}

/// `TransactionMarker` on the wire, as its `[OCMF Tab. 7]` letter.
#[cfg(feature = "serde")]
mod marker_letter {
    use ocmf::TransactionMarker;
    use serde::{Deserializer, Serializer};

    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's `with` contract passes the field by reference"
    )]
    pub(super) fn serialize<S: Serializer>(
        marker: &TransactionMarker,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_char(marker.letter())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<TransactionMarker, D::Error> {
        let text = <std::borrow::Cow<'_, str> as serde::Deserialize>::deserialize(deserializer)?;
        Ok(TransactionMarker::parse(&text))
    }
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
    /// `None` for a register whose code says nothing about direction — which is
    /// not the same as import, and a caller that needs a direction has to get it
    /// from elsewhere and know that it did.
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
    pub compensated_loss: Option<Energy>,
    /// The OBIS register the energy was taken from, canonically spelled.
    pub register: Option<String>,
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
    pub const fn is_billable(&self) -> bool {
        self.billable_energy.is_some()
    }

    /// Whether a time-priced tariff may be applied to this session.
    #[must_use]
    pub const fn is_billable_for_time(&self) -> bool {
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
/// use emob_eichrecht::chain;
/// # let begin_raw = r#"OCMF|{"FV":"1.0","PG":"T1","MS":"M1","RD":[{"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":10.0,"RI":"01-00:B2.08.00*FF","RU":"kWh","ST":"G"}]}|{"SD":"00"}"#;
/// # let end_raw = r#"OCMF|{"FV":"1.0","PG":"T2","MS":"M1","RD":[{"TM":"2026-01-02T11:00:00,000+0100 S","TX":"E","RV":25.0,"RI":"01-00:B2.08.00*FF","RU":"kWh","ST":"G"}]}|{"SD":"00"}"#;
///
/// let begin = ocmf::Record::parse(begin_raw)?;
/// let end = ocmf::Record::parse(end_raw)?;
/// let report = chain::validate(&[begin, end]);
///
/// if let Some(energy) = report.billable_energy {
///     println!("bill {energy}");
/// } else {
///     for reason in report.reasons() {
///         eprintln!("blocked: {reason}");
///     }
/// }
/// # Ok::<(), ocmf::ParseError>(())
/// ```
#[must_use]
pub fn validate(records: &[Record<'_>]) -> ChainReport {
    let refs: Vec<&Record<'_>> = records.iter().collect();
    validate_refs(&refs)
}

/// The same, over borrowed records — what [`crate::Evidence`] holds.
#[must_use]
pub fn validate_refs(records: &[&Record<'_>]) -> ChainReport {
    let sequence = session::validate_refs(records);
    let mut findings: Vec<ChainFinding> = sequence
        .findings()
        .iter()
        .cloned()
        .map(ChainFinding::Sequence)
        .collect();

    if records.is_empty() {
        return ChainReport {
            findings,
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

    check_error_flag_vocabulary(records, &mut findings);
    let identification = check_identification(records, &mut findings);
    let Energetics {
        energy,
        register,
        direction,
        compensated_loss,
        started_at,
        ended_at,
    } = billable_register(&sequence, records, &mut findings);

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
        signing_component: signing_component_of(records),
        timeline: timeline_of(records),
        started_at,
        ended_at,
    }
}

/// The signing component the chain belongs to — `MS`, or `GS` where the record
/// carries no meter serial.
///
/// `ocmf::session` has already refused a sequence whose source changes, so the
/// first record's answer is the chain's.
fn signing_component_of(records: &[&Record<'_>]) -> Option<String> {
    records.first().and_then(|record| {
        let payload = record.payload();
        payload
            .meter_serial()
            .or_else(|| payload.gateway_serial())
            .map(alloc_string)
    })
}

fn alloc_string(s: &str) -> String {
    s.to_owned()
}

/// Every transaction marker the readings carried, in signed order.
///
/// Unconditional: the timeline is what the records *say*, and a chain that does
/// not bill still has one — a dispute about an occupancy fee is exactly the case
/// where the fee was refused and somebody wants to know what the meter recorded.
fn timeline_of(records: &[&Record<'_>]) -> Vec<SignedMarker> {
    records
        .iter()
        .flat_map(|record| {
            let pagination = record.payload().pagination().map_or(0, |p| p.number());
            record
                .payload()
                .readings()
                .iter()
                .filter_map(move |reading| {
                    let marker = reading.transaction()?;
                    let at = instant_of(reading.time()?)?;
                    Some(SignedMarker {
                        pagination,
                        at,
                        marker,
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// An `EF` character this build does not know `[OCMF Tab. 7, EF]`.
///
/// The specification defines `E` and `t`. Anything else is a statement the meter
/// made and this build cannot read, and it takes everything away — a future
/// revision must not be able to widen what gets billed by adding a character.
fn check_error_flag_vocabulary(records: &[&Record<'_>], findings: &mut Vec<ChainFinding>) {
    for record in records {
        for reading in record.payload().readings() {
            let flags = reading.error_flags().as_str();
            if !flags.is_empty() && flags.chars().any(|c| c != 'E' && c != 't') {
                findings.push(ChainFinding::UnknownErrorFlag {
                    flags: flags.to_owned(),
                });
            }
        }
    }
}

/// The user assignment across the chain: the weakest level anybody asserted,
/// and a finding when it changed.
///
/// `ocmf::session` already reports a level that is an *error*
/// `[OCMF Tab. 11]`. What it does not report is a level that **changed** — a
/// session identified one way at its start and another at its end is one whose
/// records cannot all be about the same authorisation, and that is a question
/// about who to bill rather than about the sequence.
///
/// Only records that actually carry an assignment contribute. OCMF scopes its
/// abbreviation rules to the readings inside a record, not to the
/// identification section, and stations in the field routinely put the section
/// on the opening record only — so a record without one is silent rather than
/// contradicting.
fn check_identification(
    records: &[&Record<'_>],
    findings: &mut Vec<ChainFinding>,
) -> Option<IdentificationStrength> {
    let mut first: Option<String> = None;
    let mut weakest: Option<IdentificationStrength> = None;

    for record in records {
        let Some(level) = record.payload().identification_level() else {
            continue;
        };
        let written = level.as_str().to_owned();
        match &first {
            None => first = Some(written.clone()),
            Some(expected) if *expected != written => {
                findings.push(ChainFinding::IdentificationChanged {
                    expected: expected.clone(),
                    found: written.clone(),
                });
            }
            Some(_) => {}
        }
        let strength = strength_of(level);
        weakest = Some(match weakest {
            Some(current) => current.min(strength),
            None => strength,
        });
    }
    weakest
}

/// How strongly a level says the user was identified.
///
/// `[OCMF Tab. 11]` grades the assignment and this crate's scale grades the
/// *evidence*, so the mapping is a judgement rather than a rename — which is
/// why it is one function with the table beside it rather than a `From` impl
/// somebody could mistake for a definition.
fn strength_of(level: IdentificationLevel<'_>) -> IdentificationStrength {
    match level {
        // The assignment was checked against something the operator does not
        // control: a certificate, a signed contract identifier.
        IdentificationLevel::Hearsay => IdentificationStrength::Hearsay,
        IdentificationLevel::Trusted => IdentificationStrength::Trusted,
        IdentificationLevel::Verified => IdentificationStrength::Verified,
        IdentificationLevel::Certified => IdentificationStrength::Certified,
        IdentificationLevel::Secure => IdentificationStrength::Secure,
        // `NONE`, every error state — which `ocmf::session` has already
        // reported as a finding — and a level the table does not define. Each
        // is a claim nothing supports, and the weakest possible answer is the
        // one that cannot over-state the evidence.
        // `NONE`, every error state — which `ocmf::session` has already
        // reported as a finding — a level the table does not define, and any
        // level a later revision adds. Each is a claim nothing supports, and
        // the weakest possible answer is the one that cannot over-state the
        // evidence.
        _ => IdentificationStrength::None,
    }
}

/// What the register said.
struct Energetics {
    energy: Option<Energy>,
    register: Option<String>,
    direction: Option<Direction>,
    compensated_loss: Option<Energy>,
    started_at: Option<time::OffsetDateTime>,
    ended_at: Option<time::OffsetDateTime>,
}

/// Choose the register this session bills on, and judge it.
///
/// `ocmf::session` computes a [`RegisterTotal`](ocmf::session::RegisterTotal)
/// per register, which is the only way the question has an answer on a meter
/// that interleaves import and export readings. What it does not do — because it
/// is a billing decision — is pick **which** of them an invoice is for, refuse a
/// register the specification reserved and never defined, or refuse a reading
/// that is not in an energy unit at all.
fn billable_register(
    sequence: &SessionReport,
    records: &[&Record<'_>],
    findings: &mut Vec<ChainFinding>,
) -> Energetics {
    let (started_at, ended_at) = session_bounds(records);
    let loss = check_loss_compensation(records, findings);

    // The first energy register the sequence measured. A chain that measured
    // two — an import and an export on one meter — is one whose *caller* has to
    // say which it is billing, and `Evidence` splits them by direction before
    // it gets here.
    let Some(total) = sequence.totals().first() else {
        findings.push(ChainFinding::NoBillableEnergy);
        return Energetics {
            energy: None,
            register: None,
            direction: None,
            compensated_loss: loss,
            started_at,
            ended_at,
        };
    };

    let unit = ocmf::Unit::parse(&total.unit);
    if !unit.is_energy() {
        findings.push(ChainFinding::NotAnEnergyUnit {
            unit: total.unit.clone(),
        });
    }

    let code = ObisCode::parse(&total.obis);
    let register = code.as_ref().map(ocmf::ObisCode::canonical);
    let kind = code.as_ref().map(ObisCode::register);
    if kind == Some(Register::Reserved) {
        findings.push(ChainFinding::ReservedRegister {
            register: total.obis.clone(),
        });
    }
    let direction = kind.and_then(Register::is_import).map(|import| {
        if import {
            Direction::Import
        } else {
            Direction::Export
        }
    });

    // `RV` is in `RU`, and `RU` is `Wh` on ordinary German hardware. Converting
    // here, once, is what keeps a `billable_energy` in kWh from sitting beside a
    // figure a thousand times too large.
    let energy = energy_in(total.delta, &total.unit);

    Energetics {
        energy,
        register,
        direction,
        compensated_loss: loss,
        started_at,
        ended_at,
    }
}

/// A decimal in `RU`, as an [`Energy`] in kWh.
fn energy_in(value: Decimal, unit: &str) -> Option<Energy> {
    match ocmf::Unit::parse(unit) {
        ocmf::Unit::KWh => Energy::from_kwh(value).ok(),
        ocmf::Unit::Wh => Energy::from_wh(value).ok(),
        _ => None,
    }
}

/// The first and last instant the readings carry.
fn session_bounds(
    records: &[&Record<'_>],
) -> (Option<time::OffsetDateTime>, Option<time::OffsetDateTime>) {
    let instants: Vec<time::OffsetDateTime> = records
        .iter()
        .flat_map(|record| record.payload().readings())
        .filter_map(|reading| reading.time().and_then(instant_of))
        .collect();
    (
        instants.iter().min().copied(),
        instants.iter().max().copied(),
    )
}

/// The cable loss this session compensated, and the two ways it can be
/// unreadable `[OCMF Tab. 7, CL]`.
fn check_loss_compensation(
    records: &[&Record<'_>],
    findings: &mut Vec<ChainFinding>,
) -> Option<Energy> {
    let mut opening: Option<Decimal> = None;
    let mut closing: Option<(Decimal, String)> = None;

    for record in records {
        for reading in record.payload().readings() {
            let Some(loss) = reading.cumulated_loss() else {
                continue;
            };
            let value = loss.value();

            // `CL` is a compensation applied to an accumulating energy
            // register. Reported against anything else it is a number nobody
            // can trace to a value.
            let accumulates = reading
                .obis()
                .map(ObisCode::register)
                .and_then(|kind| kind.is_import().map(|_| kind))
                .is_some();
            if !accumulates {
                findings.push(ChainFinding::LossOnNonAccumulationRegister {
                    register: reading
                        .obis()
                        .map_or_else(|| "(absent)".to_owned(), |c| c.as_str().to_owned()),
                });
            }

            if reading.transaction() == Some(TransactionMarker::Begin) {
                opening = Some(value);
                if !value.is_zero() {
                    findings.push(ChainFinding::LossNotResetAtBegin {
                        cumulated: value.to_string(),
                    });
                }
            }
            let unit = reading
                .unit()
                .map_or_else(|| "kWh".to_owned(), |u| u.as_str().to_owned());
            closing = Some((value, unit));
        }
    }

    let (end, unit) = closing?;
    energy_in(end - opening.unwrap_or(Decimal::ZERO), &unit)
}

/// A `TM` as an instant, in the offset the meter wrote.
///
/// The offset is kept rather than normalised to UTC: it is what the station
/// stated about its own clock, and the settlement split reads the instants back
/// in whatever frame they arrive in.
fn instant_of(time: ocmf::OcmfTime) -> Option<time::OffsetDateTime> {
    let offset = time::UtcOffset::from_whole_seconds(i32::from(time.offset_minutes) * 60).ok()?;
    time::OffsetDateTime::from_unix_timestamp(time.unix_seconds())
        .ok()
        .map(|at| at.to_offset(offset))
}

/// The meter state a reading has to be in before anything is billed
/// `[OCMF Tab. 10]`, `[MessEG §33]`.
///
/// Re-exported so a caller reading a single record can ask the same question
/// the chain asks, without depending on `ocmf` directly.
#[must_use]
pub const fn is_billable_state(state: MeterState) -> bool {
    state.is_ok()
}

/// Whether a clock supports a duration a tariff may charge for
/// `[OCMF Tab. 19]`.
///
/// `S` (synchronised) and `R` (relative accounting from a calibration-law
/// accurate duration) qualify; `U` (unknown) and `I` (informative) do not.
#[must_use]
pub const fn is_billable_clock(status: TimeStatus) -> bool {
    matches!(status, TimeStatus::Synchronized | TimeStatus::Relative)
}
