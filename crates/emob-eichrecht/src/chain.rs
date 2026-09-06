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
//! Nine rules that are about billing rather than about the format — one row per
//! variant of [`ChainFinding`] that is not a [`ChainFinding::Sequence`], because
//! a rule the enum can report and the table cannot is a rule nobody reviewing
//! this list knows about:
//!
//! | Rule | Source | Disqualifies |
//! |---|---|---|
//! | The billed register is not one `[OCMF Tab. 25]` reserved and never defined | `[OCMF Tab. 25]` | energy |
//! | The reading is in an energy unit at all | `[OCMF Tab. 7, RU]` | energy |
//! | …and both ends of the subtraction are counted in the **same** one | `[OCMF Tab. 7, RU]` | energy |
//! | Only one **direction** moved, so which register the session is has an answer | `[OCMF Tab. 25]` | energy |
//! | Cable-loss compensation is reported against an accumulation register | `[OCMF Tab. 7, CL]` | both |
//! | …and is reset at `TX=B`, so the session's own loss is `CL_end` | `[OCMF Tab. 7, CL]` | both |
//! | The identification level does not change mid-session | `[OCMF Tab. 11]` | both |
//! | An `EF` character this build does not know | `[OCMF Tab. 7, EF]` | both |
//! | There is a usable **pair** of readings on one register to subtract at all | `[OCMF Tab. 7]` | energy |

use emob_core::{Direction, Energy, IdentificationStrength};
use ocmf::obis::Register;
use ocmf::session::{self, Finding, SessionReport};
use ocmf::{IdentificationLevel, ObisCode, Record, TransactionMarker};
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

    /// The two ends of the subtraction are counted in different units
    /// `[OCMF Tab. 7, RU]`.
    ///
    /// `RU` carries forward only **within** a record, so a chain opening in
    /// `kWh` and closing in `Wh` is something a station can emit — and
    /// `end − begin` is then not a difference at all. The energy comes out a
    /// thousand times wrong in whichever direction the reader guessed, with a
    /// unit label taken from one of the two readings and true of neither.
    ///
    /// Refused rather than converted. The two spellings are exactly
    /// inter-convertible and that is the trap: a station that changes the unit it
    /// counts in mid-transaction has done something no conforming meter does, and
    /// silently repairing it would bill a register nobody can reproduce
    /// `[MessEG §33]`. The clock is untouched, so it takes the energy and leaves
    /// the duration.
    ///
    /// Raised once per register rather than once per reading, for the reason the
    /// two `CL` findings are: a quarter-hourly session carries ninety-six of
    /// them.
    UnitChanged {
        /// The register it was measured on, canonically spelled.
        register: String,
        /// The unit the chain opened in, as written.
        expected: String,
        /// The unit that turned up, as written.
        found: String,
    },

    /// The chain measured energy in **both** directions, and only one of them
    /// can be the session `[OCMF Tab. 25]`.
    ///
    /// A bidirectional meter reports an import register and an export register,
    /// and `[OCMF Tab. 25]` puts the direction in the OBIS code so the two can
    /// never be confused. What it does not say is which of them a session is
    /// *for* — and the whole workspace rests on import and export never netting,
    /// so a chain that bills the first register it saw has silently thrown the
    /// other one away: a driver's discharge that no record downstream mentions,
    /// because nothing was refused and nothing was noted.
    ///
    /// Raised only where the other direction's register actually **moved**. A
    /// bidirectional charger doing an ordinary import reports a `C` register
    /// that stayed still, and refusing that would refuse the hardware for being
    /// capable.
    ///
    /// Disqualifies the energy and leaves the clock: which quantity the session
    /// is remains a question somebody has to answer, and this crate will not
    /// answer it by taking whichever register was listed first.
    MixedDirections {
        /// The register the chain would otherwise have billed.
        billed: String,
        /// The register measuring the other direction.
        also: String,
        /// What that other register moved by, as written.
        moved: String,
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
            | Self::UnitChanged { .. }
            | Self::MixedDirections { .. }
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
/// decision.
///
/// `Finding` is `#[non_exhaustive]`, so an exhaustive `match` is not available
/// and a variant a later release adds cannot be a compile error here. What is
/// available is the **direction** of the fallback: the three narrow answers
/// below are the exhaustive list of what is *not* fatal, and everything else —
/// including anything unrecognised — takes both quantities away. A new upstream
/// check can therefore only ever narrow what this build bills, never widen it.
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
            Self::UnitChanged {
                register,
                expected,
                found,
            } => write!(
                f,
                "the readings on {register} are counted in more than one unit: {expected} then {found}. RU carries forward only within a record [OCMF Tab. 7], so the difference between the two ends is not a quantity"
            ),
            Self::MixedDirections {
                billed,
                also,
                moved,
            } => write!(
                f,
                "the chain measured energy in both directions — {billed} and {also}, which moved by {moved} — and only one of them is this session [OCMF Tab. 25]. Import and export never net, so which register is billed is a question a caller has to answer rather than one this crate answers by taking the first"
            ),
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
/// time a vehicle is connected and not charging per minute, and the alternative
/// source for that interval is OCPP's `chargingState` — a protocol field,
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
    /// `CL` accumulates across the transaction and "must be reset at `TX=B`"
    /// `[OCMF Tab. 7, CL]`, so the session's own loss is the closing value —
    /// computed as `CL_end − CL_begin` so that a meter which failed to reset it
    /// still yields a difference, beside the [`ChainFinding::LossNotResetAtBegin`]
    /// that takes the record out of billing. Not subtracted from anything — the
    /// compensation is already inside `RV` — but carried, because a partner
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
///
/// **Once per distinct flag string.** `EF` carries forward, so a fault raised at
/// the first reading is restated by every reading after it: a quarter-hourly
/// session reported the same unreadable character ninety-six times. That is the
/// rule the two `CL` findings and `RatingNote::Unpriced` already keep — ninety-six
/// identical notes are one fact reported ninety-six times, and a queue nobody
/// reads (D296). A *second*, different flag string is a second fact and is
/// reported.
fn check_error_flag_vocabulary(records: &[&Record<'_>], findings: &mut Vec<ChainFinding>) {
    let mut reported: Vec<&str> = Vec::new();
    for record in records {
        for reading in record.payload().readings() {
            let flags = reading.error_flags().as_str();
            if flags.is_empty() || flags.chars().all(|c| c == 'E' || c == 't') {
                continue;
            }
            if reported.contains(&flags) {
                continue;
            }
            reported.push(flags);
            findings.push(ChainFinding::UnknownErrorFlag {
                flags: flags.to_owned(),
            });
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

    // The first energy register the sequence measured. A chain that measured
    // two is only unambiguous while they agree about the direction, which is
    // `check_one_direction`'s question below — and a meter reporting a lifetime
    // and a transaction register of the same direction states one quantity
    // twice.
    let Some(total) = sequence.totals().first() else {
        findings.push(ChainFinding::NoBillableEnergy);
        return Energetics {
            energy: None,
            register: None,
            direction: None,
            // Nothing was billed, so no register's compensation is this
            // chain's. Reading one anyway would attach a figure to a quantity
            // that does not exist.
            compensated_loss: None,
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

    // …and `RegisterTotal` labels its delta with the unit of the **begin**
    // reading alone, so the subtraction underneath it has never been asked
    // whether the two ends were counted in the same thing.
    check_unit_consistency(records, register.as_deref(), &total.unit, findings);

    // …and neither the format nor this function has asked whether the *other*
    // registers measured the other direction. Taking the first one is what this
    // used to do, silently.
    check_one_direction(sequence, register.as_deref(), direction, findings);

    // `RV` is in `RU`, and `RU` is `Wh` on ordinary German hardware. Converting
    // here, once, is what keeps a `billable_energy` in kWh from sitting beside a
    // figure a thousand times too large.
    let energy = energy_in(total.delta, &total.unit);

    // The compensation belongs to **this** register. A meter that reports two —
    // a total and a transaction register, an import and an export — carries a
    // `CL` on each, and a sweep that took the last one it saw subtracted an
    // opening value from one register out of a closing value from another. The
    // figure that came out was not a quantity at all (D242).
    let loss = check_loss_compensation(records, register.as_deref(), &total.unit, findings);

    Energetics {
        energy,
        register,
        direction,
        compensated_loss: loss,
        started_at,
        ended_at,
    }
}

/// Whether exactly one **direction** moved across this chain `[OCMF Tab. 25]`.
///
/// # The comment that was not true
///
/// This function's caller used to say *"a chain that measured two — an import
/// and an export on one meter — is one whose caller has to say which it is
/// billing, and `Evidence` splits them by direction before it gets here"*.
/// [`crate::Evidence::assemble`] does no such thing: it hands every record to
/// [`validate`] and takes one answer back. So a well-formed chain carrying
/// `01-00:B2.08.00*FF` at +18 kWh and `01-00:C2.08.00*FF` at +5 kWh — a V2G
/// session, both registers properly marked, every signature valid — reported
/// **no finding at all**, billed the 18, and dropped the 5 on the floor.
///
/// Not a netting bug: nothing was subtracted. A worse one, in this workspace's
/// terms — a measured quantity that vanished with no reason attached, out of the
/// one crate whose entire promise is that a value which does not bill says why.
///
/// # Only where the other direction moved
///
/// A bidirectional charger doing an ordinary import reports a `C` register that
/// stood still all session, and so does a meter that carries both a lifetime and
/// a transaction register of the same direction. Refusing either would refuse
/// hardware for being capable. The ambiguity is a register that moved, in the
/// direction the billed one is not.
fn check_one_direction(
    sequence: &SessionReport,
    billed: Option<&str>,
    direction: Option<Direction>,
    findings: &mut Vec<ChainFinding>,
) {
    let Some(direction) = direction else {
        // A register whose code states no direction states none about the
        // others either, and `ChainReport::direction` already says so.
        return;
    };
    for total in sequence.totals().iter().skip(1) {
        if total.delta.is_zero() {
            continue;
        }
        let other = ObisCode::parse(&total.obis)
            .as_ref()
            .map(ObisCode::register)
            .and_then(Register::is_import)
            .map(|import| {
                if import {
                    Direction::Import
                } else {
                    Direction::Export
                }
            });
        if other.is_some_and(|other| other != direction) {
            findings.push(ChainFinding::MixedDirections {
                billed: billed.unwrap_or("(unparsed register)").to_owned(),
                also: total.obis.clone(),
                moved: total.delta.to_string(),
            });
            // One finding per chain: the question is which register the session
            // is, and a third register does not make it a different question.
            return;
        }
    }
}

/// Whether both ends of the subtraction were counted in the same unit
/// `[OCMF Tab. 7, RU]`.
///
/// # Why `ocmf` cannot answer this and this crate has to
///
/// [`RegisterTotal`](ocmf::session::RegisterTotal) takes its `unit` from the
/// **begin** reading and its `delta` from `end − begin`, and compares the two
/// readings' units nowhere: that is arithmetic over what the records say, and
/// whether the arithmetic is a *quantity* is a billing question. `RU` carries
/// forward only within a record, so a chain opening `2935.600 kWh` and closing
/// `2965100 Wh` — the same physical 29.5 kWh — bills **2 962 164.4 kWh** with
/// nothing objecting. The energy is a million times the truth, the label is
/// `kWh`, and every signature verifies.
///
/// # Silent is not contradicting
///
/// Only readings that **state** a unit are compared. A reading that omits `RU`
/// with nothing in its own record to carry from says nothing, and real meters
/// omit fields the tables mark mandatory — 89 % of the S.A.F.E. corpus omits an
/// `MS`. Refusing an absence would refuse hardware that has done nothing wrong,
/// which is the same reasoning [`check_identification`] already applies to a
/// record carrying no identification section.
///
/// The register total's own unit is the first claim, so a chain whose *only*
/// stated unit sits on the begin reading is consistent by construction.
fn check_unit_consistency(
    records: &[&Record<'_>],
    billed: Option<&str>,
    total_unit: &str,
    findings: &mut Vec<ChainFinding>,
) {
    for record in records {
        for reading in record.payload().readings() {
            // Only the register this chain bills. A meter reporting an import
            // and an export register reports two series, and one of them
            // changing its unit says nothing about the other.
            if let Some(billed) = billed
                && reading.obis().map(ObisCode::canonical).as_deref() != Some(billed)
            {
                continue;
            }
            let Some(unit) = reading.unit() else { continue };
            let found = unit.as_str();
            if found == total_unit {
                continue;
            }
            findings.push(ChainFinding::UnitChanged {
                register: billed.unwrap_or("(unparsed register)").to_owned(),
                expected: total_unit.to_owned(),
                found: found.to_owned(),
            });
            // One finding per register: the readings after the change are the
            // same fact restated, and a quarter-hourly session carries
            // ninety-six of them.
            return;
        }
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

/// The cable loss **this chain's own register** compensated, and the two ways
/// it can be unreadable `[OCMF Tab. 7, CL]`.
///
/// # Which register's, and why that had to be said
///
/// `CL` is a property of the register it sits beside — "given in the same unit
/// as `RV`", accumulating across the transaction, reset at `TX=B`. A meter that
/// reports two registers reports two `CL` series, and a sweep that kept the last
/// value it saw took a closing value from one register against an opening value
/// from another. The difference is not a small error, it is not a quantity: it
/// is `CL_end(C2) − CL_begin(B2)`, and nothing in the format says those two
/// numbers are about the same thing (D242).
///
/// So the readings are filtered to `billed` — the canonical spelling of the
/// register [`billable_register`] chose — and a chain with no billable register
/// has no compensation to report either. `None` for `billed` is a chain whose
/// register would not parse, and there the sweep falls back to every reading,
/// because "which register" has no answer and reporting nothing would hide a
/// `CL` a dispute turns on.
///
/// # One finding per register, not one per reading
///
/// A session sampled every quarter hour carries ninety-six readings. Reporting
/// the same fault ninety-six times is a queue nobody reads, and it is the same
/// rule the rating engine keeps for an unpriced dimension: the finding is about
/// the register, so it is raised once for it.
///
/// # …and in the unit the register was counted in
///
/// `CL` "is given in the same unit as `RV` which is specified in `RU`", so the
/// unit is the **billed register's** rather than whichever reading happened to
/// close the sweep. Reading it off the closing reading and falling back to `kWh`
/// where that reading stated none put a `Wh` figure on the record labelled as
/// kilowatt-hours — a thousand-fold error in the one number that exists to
/// explain the energy beside it, on a chain that bills perfectly well.
/// [`check_unit_consistency`] has already refused a register whose readings
/// disagree about the unit, so this is the register's one answer rather than a
/// choice between two.
fn check_loss_compensation(
    records: &[&Record<'_>],
    billed: Option<&str>,
    total_unit: &str,
    findings: &mut Vec<ChainFinding>,
) -> Option<Energy> {
    let mut opening: Option<Decimal> = None;
    let mut closing: Option<Decimal> = None;
    let mut not_an_accumulation: Option<String> = None;
    let mut not_reset: Option<Decimal> = None;

    for record in records {
        for reading in record.payload().readings() {
            let Some(loss) = reading.cumulated_loss() else {
                continue;
            };
            let code = reading.obis();
            // Only this chain's own register contributes. See above.
            if let Some(billed) = billed
                && code.map(ObisCode::canonical).as_deref() != Some(billed)
            {
                continue;
            }
            let value = loss.value();

            // `CL` is a compensation applied to an accumulating energy
            // register. Reported against anything else it is a number nobody
            // can trace to a value.
            let accumulates = code
                .as_ref()
                .is_some_and(|code| is_accumulation_register(code));
            if !accumulates && not_an_accumulation.is_none() {
                not_an_accumulation =
                    Some(code.map_or_else(|| "(absent)".to_owned(), |c| c.as_str().to_owned()));
            }

            if reading.transaction() == Some(TransactionMarker::Begin) {
                opening = Some(value);
                if !value.is_zero() && not_reset.is_none() {
                    not_reset = Some(value);
                }
            }
            closing = Some(value);
        }
    }

    if let Some(register) = not_an_accumulation {
        findings.push(ChainFinding::LossOnNonAccumulationRegister { register });
    }
    if let Some(cumulated) = not_reset {
        findings.push(ChainFinding::LossNotResetAtBegin {
            cumulated: cumulated.to_string(),
        });
    }

    let end = closing?;
    energy_in(end - opening.unwrap_or(Decimal::ZERO), total_unit)
}

/// Whether an OBIS code names a register whose reading **accumulates**
/// `[OCMF Tab. 7, CL]`.
///
/// The question `CL` turns on — *"may be reported only when RI is indicating an
/// accumulation register reading"* — and it used to be asked as *"does the code
/// state a direction"*, which is a different question and wrong in both
/// directions.
///
/// It refused a **manufacturer** register, because `[OCMF Tab. 25]` classifies
/// by the `C` group alone and anything outside its own range is `Other` with no
/// direction. `EICHRECHT.md` says the opposite in as many words — *"a
/// manufacturer register nobody recognises is still evidence and still bills"* —
/// and a `CL` beside one took **both** quantities away.
///
/// And it accepted an **instantaneous** one. `1-0:1.7.0*255` is active power,
/// not energy, and it classifies as `ActiveEnergyImport` because that is decided
/// by `C = 1`; what separates a running total from a spot reading is the `D`
/// group, where **8** is the time integral. `1.8.0` accumulates and `1.7.0` does
/// not, and the two differ in the digit this never read (D296).
///
/// So: any of `[OCMF Tab. 25]`'s own registers, which the table *defines* as
/// totals, or an IEC code whose `D` group is 8.
fn is_accumulation_register(code: &ObisCode<'_>) -> bool {
    match code.register() {
        Register::TotalImportMains
        | Register::TotalImportDevice
        | Register::TransactionImportMains
        | Register::TransactionImportDevice
        | Register::TotalExportMains
        | Register::TotalExportDevice
        | Register::TransactionExportMains
        | Register::TransactionExportDevice => true,
        // `[OCMF Tab. 25]` claimed these codes and never said what they measure,
        // so nothing can be said about what a compensation beside one means
        // either. `ChainFinding::ReservedRegister` is the finding that names it.
        Register::Reserved => false,
        // Everything from the ordinary IEC grammar, including the classic
        // `1.8.0` the S.A.F.E. reference record bills on and every manufacturer
        // code: the `D` group decides.
        // `Register` is `#[non_exhaustive]`, so the ordinary IEC grammar and
        // anything a later release adds land here together — and the `D` group
        // is the question either way.
        _ => code.processing() == "8",
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A record with a reading on one register, carrying a cumulated loss.
    fn record(pagination: &str, tx: &str, obis: &str, rv: &str, cl: &str) -> String {
        format!(
            r#"OCMF|{{"FV":"1.0","PG":"{pagination}","MS":"M1","RD":[{{"TM":"2026-01-02T10:00:00,000+0100 S","TX":"{tx}","RV":{rv},"RI":"{obis}","RU":"kWh","CL":{cl},"ST":"G"}}]}}|{{"SD":"00"}}"#
        )
    }

    /// `[OCMF Tab. 7, CL]` — a meter reporting **two** registers reports two
    /// cumulated-loss series, and they are not one series.
    ///
    /// The import register's loss runs 0 → 0.4 across the transaction; the
    /// export register's runs 0 → 9.0. Taking the last `CL` seen against the
    /// first `TX=B` seen crosses them and reports 9.0 as the import session's
    /// cable loss — a figure attached to the one quantity it is supposed to
    /// explain, and more than twenty times the truth (D242).
    #[test]
    fn cable_loss_belongs_to_the_register_that_was_billed() {
        let raw = [
            record("T1", "B", "01-00:B2.08.00*FF", "10.0", "0.0"),
            record("T2", "C", "01-00:C2.08.00*FF", "0.0", "0.0"),
            record("T3", "E", "01-00:B2.08.00*FF", "25.0", "0.4"),
            record("T4", "E", "01-00:C2.08.00*FF", "3.0", "9.0"),
        ];
        let records: Vec<Record<'_>> = raw
            .iter()
            .map(|r| Record::parse(r).expect("a well-formed record"))
            .collect();

        let report = validate(&records);

        // The chain bills the first register the sequence measured, and the
        // loss it reports is that register's own.
        assert_eq!(
            report.compensated_loss,
            Some(
                Energy::from_kwh(Decimal::from_str_exact("0.4").expect("a decimal")).expect("kWh")
            ),
            "the export register's 9.0 is not this session's cable loss"
        );
    }

    /// A record with a reading whose unit is written however the caller says.
    fn reading(pagination: &str, tx: &str, rv: &str, ru: Option<&str>, minute: u8) -> String {
        let unit = ru.map_or_else(String::new, |u| format!(r#""RU":"{u}","#));
        format!(
            r#"OCMF|{{"FV":"1.0","PG":"{pagination}","MS":"M1","RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{rv},"RI":"01-00:B2.08.00*FF",{unit}"ST":"G"}}]}}|{{"SD":"00"}}"#
        )
    }

    fn report_over(raw: &[String]) -> ChainReport {
        let records: Vec<Record<'_>> = raw
            .iter()
            .map(|r| Record::parse(r).expect("a well-formed record"))
            .collect();
        validate(&records)
    }

    /// `[OCMF Tab. 7, RU]` — **the two ends have to be the same quantity.**
    ///
    /// `RU` carries forward only *within* a record, so a chain opening in kWh
    /// and closing in Wh is something a station can emit. `RegisterTotal` labels
    /// its delta with the unit of the *begin* reading and compares the two
    /// nowhere, because that is arithmetic and this is a billing question — so
    /// the same physical 29.5 kWh came out as **2 962 164.4 kWh**, with a `kWh`
    /// label, no finding, and every signature verifying.
    ///
    /// The rule was written once (D36) and lost when the format moved to `ocmf`
    /// (D184); the documentation went on claiming it for four milestones, which
    /// is rule 1 pointed at a check rather than at a `pub fn` (D290).
    #[test]
    fn the_two_ends_have_to_be_counted_in_the_same_unit() {
        let report = report_over(&[
            reading("T1", "B", "2935.600", Some("kWh"), 0),
            reading("T2", "E", "2965100", Some("Wh"), 30),
        ]);

        assert_eq!(
            report.billable_energy, None,
            "a difference of two units is not a quantity"
        );
        let found = report
            .findings
            .iter()
            .find(|f| matches!(f, ChainFinding::UnitChanged { .. }))
            .expect("the unit change is reported");
        assert_eq!(found.disqualifies(), Disqualifies::Energy);
        // …and it takes the energy only: the clock said nothing.
        assert!(report.billable_duration.is_some());
        assert!(found.to_string().contains("kWh then Wh"), "{found}");
    }

    /// One finding, however many readings restate the unit.
    ///
    /// The same rule the two `CL` findings keep: a quarter-hourly session
    /// carries ninety-six readings, and reporting one fact ninety-six times is a
    /// queue nobody reads.
    #[test]
    fn a_changed_unit_is_reported_once_per_register() {
        let report = report_over(&[
            reading("T1", "B", "2935.600", Some("kWh"), 0),
            reading("T2", "C", "2940000", Some("Wh"), 10),
            reading("T3", "C", "2950000", Some("Wh"), 20),
            reading("T4", "E", "2965100", Some("Wh"), 30),
        ]);
        assert_eq!(
            report
                .findings
                .iter()
                .filter(|f| matches!(f, ChainFinding::UnitChanged { .. }))
                .count(),
            1
        );
    }

    /// A reading that states **no** unit is silent, not contradicting.
    ///
    /// Real meters omit fields the tables mark mandatory — 89 % of the S.A.F.E.
    /// corpus omits an `MS` — so an absence may not be read as a second claim.
    /// The same reasoning `check_identification` applies to a record carrying no
    /// identification section.
    #[test]
    fn a_reading_that_states_no_unit_says_nothing() {
        let report = report_over(&[
            reading("T1", "B", "2935.600", Some("kWh"), 0),
            reading("T2", "E", "2965.100", None, 30),
        ]);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| matches!(f, ChainFinding::UnitChanged { .. })),
            "{:?}",
            report.findings
        );
        assert_eq!(
            report.billable_energy.map(|e| e.to_string()),
            Some("29.500 kWh".to_owned())
        );
    }

    /// A `CL` is stated in the **register's** unit `[OCMF Tab. 7, CL]`.
    ///
    /// A meter counting in `Wh` reports its cable loss in `Wh`, and reading the
    /// unit off whichever reading closed the sweep — falling back to `kWh` where
    /// it stated none — put a figure a thousand times too large on the one
    /// number that exists to explain the energy beside it.
    #[test]
    fn a_cable_loss_is_read_in_the_registers_own_unit() {
        let in_wh = |pagination: &str, tx: &str, rv: &str, cl: &str, minute: u8| {
            format!(
                r#"OCMF|{{"FV":"1.0","PG":"{pagination}","MS":"M1","RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{rv},"RI":"01-00:B2.08.00*FF","RU":"Wh","CL":{cl},"ST":"G"}}]}}|{{"SD":"00"}}"#
            )
        };
        let report = report_over(&[
            in_wh("T1", "B", "2935600", "0.0", 0),
            in_wh("T2", "E", "2965100", "400.0", 30),
        ]);
        assert_eq!(
            report.billable_energy.map(|e| e.to_string()),
            Some("29.500 kWh".to_owned())
        );
        assert_eq!(
            report.compensated_loss.map(|e| e.to_string()),
            Some("0.4000 kWh".to_owned()),
            "400.0 Wh of cable, not 400.0 kWh — and the meter's own scale survives"
        );
    }

    /// `[OCMF Tab. 25]` — **import and export never net, and one of them is not
    /// this session.**
    ///
    /// A bidirectional meter reports both registers. With both properly marked
    /// this chain validated with *no finding at all*, billed the import and
    /// dropped the export: not a netting bug — nothing was subtracted — but a
    /// measured quantity that vanished with no reason attached, out of the one
    /// crate whose whole promise is that a value which does not bill says why
    /// (D293).
    #[test]
    fn a_chain_that_measured_both_directions_bills_neither() {
        let both = |pagination: &str, tx: &str, import: &str, export: &str, minute: u8| {
            format!(
                r#"OCMF|{{"FV":"1.0","PG":"{pagination}","MS":"M1","RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{import},"RI":"01-00:B2.08.00*FF","RU":"kWh","ST":"G"}},{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{export},"RI":"01-00:C2.08.00*FF","RU":"kWh","ST":"G"}}]}}|{{"SD":"00"}}"#
            )
        };
        let report = report_over(&[
            both("T1", "B", "100.000", "50.000", 0),
            both("T2", "E", "118.000", "55.000", 30),
        ]);

        assert_eq!(report.billable_energy, None);
        let found = report
            .findings
            .iter()
            .find(|f| matches!(f, ChainFinding::MixedDirections { .. }))
            .expect("the second direction is reported");
        assert_eq!(found.disqualifies(), Disqualifies::Energy);
        assert!(found.to_string().contains("5.000"), "{found}");
    }

    /// …and a bidirectional charger doing an ordinary import is not refused for
    /// being capable.
    ///
    /// The `C` register is there and stood still all session. So does a meter
    /// carrying a lifetime register beside a transaction one: two registers is
    /// the ordinary shape, and the ambiguity is a register that **moved** in the
    /// direction the billed one is not.
    #[test]
    fn an_export_register_that_did_not_move_is_not_an_ambiguity() {
        let both = |pagination: &str, tx: &str, import: &str, minute: u8| {
            format!(
                r#"OCMF|{{"FV":"1.0","PG":"{pagination}","MS":"M1","RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{import},"RI":"01-00:B2.08.00*FF","RU":"kWh","ST":"G"}},{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":50.000,"RI":"01-00:C2.08.00*FF","RU":"kWh","ST":"G"}}]}}|{{"SD":"00"}}"#
            )
        };
        let report = report_over(&[
            both("T1", "B", "100.000", 0),
            both("T2", "E", "118.000", 30),
        ]);
        assert_eq!(
            report.billable_energy.map(|e| e.to_string()),
            Some("18.000 kWh".to_owned()),
            "{:?}",
            report.findings
        );
    }

    /// Every one of this module's own findings, exercised.
    ///
    /// # Why this is one test rather than six
    ///
    /// A sweep over every refusal in the workspace asked which of them a test
    /// ever *names*, and six of these did not — each one documented at length,
    /// in the crate, in its README and on the site, and none of them ever run
    /// (D296). Rule 1 says a rule with no caller is a rule not enforced;
    /// `check-reach` finds a `pub fn` nobody calls and cannot see an enum
    /// variant nothing constructs. This is that gap, closed by hand.
    ///
    /// Two of the six were **wrong**, and only running them showed it. See
    /// [`is_accumulation_register`] and [`check_error_flag_vocabulary`].
    #[test]
    fn every_register_finding_this_module_raises_can_be_reached() {
        let with = |obis: &str, ru: &str, extra: &str| {
            let reading = |pagination: &str, tx: &str, rv: &str, minute: u8| {
                format!(
                    r#"OCMF|{{"FV":"1.0","PG":"{pagination}","MS":"M1","RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{rv},"RI":"{obis}","RU":"{ru}"{extra},"ST":"G"}}]}}|{{"SD":"00"}}"#
                )
            };
            report_over(&[
                reading("T1", "B", "10.000", 0),
                reading("T2", "E", "25.000", 30),
            ])
        };
        let raises = |report: &ChainReport, want: &ChainFinding| {
            report
                .findings
                .iter()
                .any(|found| std::mem::discriminant(found) == std::mem::discriminant(want))
        };
        // A begin/end pair on one register, with the compensation reset at
        // `TX=B` the way `[OCMF Tab. 7, CL]` asks.
        let compensated = |obis: &str| {
            let reading = |pagination: &str, tx: &str, rv: &str, cl: &str, minute: u8| {
                format!(
                    r#"OCMF|{{"FV":"1.0","PG":"{pagination}","MS":"M1","RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{rv},"RI":"{obis}","RU":"kWh","CL":{cl},"ST":"G"}}]}}|{{"SD":"00"}}"#
                )
            };
            report_over(&[
                reading("T1", "B", "10.000", "0.0", 0),
                reading("T2", "E", "25.000", "0.4", 30),
            ])
        };

        // `[OCMF Tab. 25]` claimed `B4`–`BF` and never said what they measure.
        let reserved = with("01-00:B4.08.00*FF", "kWh", "");
        assert!(
            raises(
                &reserved,
                &ChainFinding::ReservedRegister {
                    register: String::new()
                }
            ),
            "{:?}",
            reserved.findings
        );
        assert_eq!(reserved.billable_energy, None);

        // `mOhm` is a lawful `RU` — a meter reporting cable resistance — and a
        // difference of two of them is not electricity.
        let ohms = with("01-00:B2.08.00*FF", "mOhm", "");
        assert!(
            raises(
                &ohms,
                &ChainFinding::NotAnEnergyUnit {
                    unit: String::new()
                }
            ),
            "{:?}",
            ohms.findings
        );

        // `CL` accumulates across the transaction and is reset at `TX=B`, so a
        // transaction opening on a non-zero one is carrying the previous
        // session's compensation into this one.
        let carried = with("01-00:B2.08.00*FF", "kWh", r#","CL":3.0"#);
        assert!(
            raises(
                &carried,
                &ChainFinding::LossNotResetAtBegin {
                    cumulated: String::new()
                }
            ),
            "{:?}",
            carried.findings
        );

        // …and it belongs beside a register that accumulates. `1.7.0` is
        // instantaneous **power**: it classifies as active-energy-import because
        // `[OCMF Tab. 25]` reads the `C` group, and what separates a running
        // total from a spot reading is the `D` group.
        let power = compensated("1-0:1.7.0*255");
        assert!(
            raises(
                &power,
                &ChainFinding::LossOnNonAccumulationRegister {
                    register: String::new()
                }
            ),
            "{:?}",
            power.findings
        );

        // …and a **manufacturer** register with a `D` of 8 is an accumulation
        // register, whatever its quantity group says. Asking for a direction
        // instead refused it, and took both quantities away from a meter that
        // had done nothing wrong.
        let vendor = compensated("1-0:129.8.0*255");
        assert!(
            vendor.findings.is_empty(),
            "a manufacturer register is still evidence and still bills: {:?}",
            vendor.findings
        );
        assert_eq!(
            vendor.compensated_loss.map(|loss| loss.to_string()),
            Some("0.4 kWh".to_owned())
        );

        // …and the last of the register-shaped ones: records that carry no
        // **pair** of readings to subtract at all. Not the empty chain, which
        // `ocmf::session` answers first and this function returns on — a chain
        // with records in it whose begin reading states no unit, so no register
        // total can be built.
        let unitless = |pagination: &str, tx: &str, rv: &str, minute: u8| {
            format!(
                r#"OCMF|{{"FV":"1.0","PG":"{pagination}","MS":"M1","RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{rv},"RI":"01-00:B2.08.00*FF","ST":"G"}}]}}|{{"SD":"00"}}"#
            )
        };
        let nothing = report_over(&[
            unitless("T1", "B", "10.000", 0),
            unitless("T2", "E", "25.000", 30),
        ]);
        assert!(
            raises(&nothing, &ChainFinding::NoBillableEnergy),
            "{:?}",
            nothing.findings
        );
        assert_eq!(nothing.billable_energy, None);
    }

    /// …and the two that are about a **reading** rather than a register.
    ///
    /// The other half of the sweep above. Split because the fixtures are different shapes, not because the claim is:
    /// every variant of `ChainFinding` that is not a `Sequence` is reached by
    /// one of the two.
    #[test]
    fn every_reading_finding_this_module_raises_can_be_reached() {
        let raises = |report: &ChainReport, want: &ChainFinding| {
            report
                .findings
                .iter()
                .any(|found| std::mem::discriminant(found) == std::mem::discriminant(want))
        };
        let with = |obis: &str, ru: &str, extra: &str| {
            let reading = |pagination: &str, tx: &str, rv: &str, minute: u8| {
                format!(
                    r#"OCMF|{{"FV":"1.0","PG":"{pagination}","MS":"M1","RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{rv},"RI":"{obis}","RU":"{ru}"{extra},"ST":"G"}}]}}|{{"SD":"00"}}"#
                )
            };
            report_over(&[
                reading("T1", "B", "10.000", 0),
                reading("T2", "E", "25.000", 30),
            ])
        };

        // An `EF` character the specification does not define might disqualify
        // either quantity, so it disqualifies both.
        let unknown = with("01-00:B2.08.00*FF", "kWh", r#","EF":"Q""#);
        assert!(
            raises(
                &unknown,
                &ChainFinding::UnknownErrorFlag {
                    flags: String::new()
                }
            ),
            "{:?}",
            unknown.findings
        );
        // …once, however many readings restate it. `EF` carries forward, so a
        // quarter-hourly session reported the same character ninety-six times.
        assert_eq!(
            unknown
                .findings
                .iter()
                .filter(|found| matches!(found, ChainFinding::UnknownErrorFlag { .. }))
                .count(),
            1
        );

        // A session identified one way at its start and another at its end is
        // one whose records cannot all be about the same authorisation.
        let identified = |pagination: &str, tx: &str, level: &str, rv: &str, minute: u8| {
            format!(
                r#"OCMF|{{"FV":"1.0","PG":"{pagination}","MS":"M1","IS":true,"IL":"{level}","IT":"ISO14443","ID":"AB","RD":[{{"TM":"2026-01-02T10:{minute:02}:00,000+0100 S","TX":"{tx}","RV":{rv},"RI":"01-00:B2.08.00*FF","RU":"kWh","ST":"G"}}]}}|{{"SD":"00"}}"#
            )
        };
        let changed = report_over(&[
            identified("T1", "B", "TRUSTED", "10.000", 0),
            identified("T2", "E", "VERIFIED", "25.000", 30),
        ]);
        assert!(
            raises(
                &changed,
                &ChainFinding::IdentificationChanged {
                    expected: String::new(),
                    found: String::new()
                }
            ),
            "{:?}",
            changed.findings
        );
    }

    /// A chain with nothing to bill reports no compensation either.
    ///
    /// A `CL` belongs to a register, and a chain that chose none has no
    /// register to attach it to — reporting one anyway would put a figure
    /// beside a quantity that does not exist.
    #[test]
    fn a_chain_with_no_billable_register_reports_no_loss() {
        let report = validate(&[]);
        assert_eq!(report.compensated_loss, None);
        assert!(report.findings.iter().any(
            |f| matches!(f, ChainFinding::Sequence(_)) || *f == ChainFinding::NoBillableEnergy
        ));
    }
}
