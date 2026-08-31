//! Checking a CDR somebody else built.
//!
//! A CDR this workspace constructs cannot fail its own arithmetic — the builder
//! refuses. A CDR that arrives from a roaming partner was built by somebody
//! else's code, and the assumptions do not transfer.
//!
//! # Report, do not reject
//!
//! [`validate`] returns every problem it finds rather than the first, and it
//! separates the ones that block settlement from the ones worth knowing about.
//! The reason is the same as everywhere else in this workspace: a partner
//! integration is debugged by seeing all of what is wrong at once, and a
//! validator that stops at the first fault turns one afternoon into six
//! round trips.
//!
//! It also never mutates. A CDR whose periods do not sum to its total is not
//! quietly fixed to sum — that would be inventing a number, on behalf of
//! somebody who will be invoiced for it.

use emob_core::Energy;

use crate::cdr::Cdr;

/// How much a finding matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Severity {
    /// Worth knowing; settlement may proceed.
    Warning,
    /// Settlement must not proceed on this record.
    Blocking,
}

/// Something wrong with a CDR.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Finding {
    /// The periods do not sum to the stated total.
    DoesNotConserve {
        /// What the periods add up to.
        periods: Energy,
        /// What the record claims.
        total: Energy,
    },
    /// The record has no periods at all.
    NoPeriods,
    /// The session ends before it starts.
    EndsBeforeItStarts,
    /// A period starts outside the session window.
    PeriodOutsideSession {
        /// Which period.
        start: time::OffsetDateTime,
    },
    /// The periods are not in time order.
    PeriodsOutOfOrder {
        /// The period that came after a later one.
        start: time::OffsetDateTime,
    },
    /// Two periods start at the same instant.
    DuplicatePeriod {
        /// The repeated start.
        start: time::OffsetDateTime,
    },
    /// No signed evidence backs the record.
    NoEvidence,
    /// Some periods were interpolated rather than measured.
    Interpolated {
        /// How many.
        count: usize,
    },
    /// The record has energy but no duration.
    NoDuration,
    /// The evidence claims a stronger identification than the authorisation
    /// path supports.
    AuthStrengthOverstated {
        /// What the path can support.
        ceiling: emob_session::IdentificationStrength,
        /// What the evidence claims.
        signed: emob_session::IdentificationStrength,
    },
}

impl Finding {
    /// How much this one matters.
    #[must_use]
    pub const fn severity(&self) -> Severity {
        match self {
            // Arithmetic and structure: a record that does not add up cannot be
            // settled at all.
            Self::DoesNotConserve { .. }
            | Self::NoPeriods
            | Self::EndsBeforeItStarts
            | Self::PeriodOutsideSession { .. }
            | Self::PeriodsOutOfOrder { .. }
            | Self::DuplicatePeriod { .. }
            | Self::NoDuration
            | Self::AuthStrengthOverstated { .. } => Severity::Blocking,

            // Missing evidence is blocking for a German energy invoice
            // `[MessEG §33]` and merely notable elsewhere, so the *finding* is
            // a warning and the decision belongs to the billing layer that
            // knows which regime applies. Reporting it as blocking here would
            // make this crate refuse perfectly lawful settlement outside
            // Germany.
            Self::NoEvidence | Self::Interpolated { .. } => Severity::Warning,
        }
    }
}

impl core::fmt::Display for Finding {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DoesNotConserve { periods, total } => write!(
                f,
                "the periods sum to {periods} but the record claims {total}"
            ),
            Self::NoPeriods => write!(f, "the record has no charging periods"),
            Self::EndsBeforeItStarts => write!(f, "the record ends before it starts"),
            Self::PeriodOutsideSession { start } => {
                write!(f, "a period at {start} lies outside the session window")
            }
            Self::PeriodsOutOfOrder { start } => {
                write!(f, "the period at {start} is out of time order")
            }
            Self::DuplicatePeriod { start } => {
                write!(f, "two periods start at {start}")
            }
            Self::NoEvidence => write!(f, "no signed evidence backs this record"),
            Self::Interpolated { count } => write!(
                f,
                "{count} period(s) were interpolated between readings rather than measured"
            ),
            Self::NoDuration => write!(f, "the record has energy but no duration"),
            Self::AuthStrengthOverstated { ceiling, signed } => write!(
                f,
                "the signed record claims {signed} identification but the authorisation path supports at most {ceiling}"
            ),
        }
    }
}

/// Everything wrong with a CDR.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// Every finding, in the order they were checked.
    pub findings: Vec<Finding>,
}

impl Report {
    /// The findings that block settlement.
    pub fn blocking(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity() == Severity::Blocking)
    }

    /// The findings that are merely worth knowing.
    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity() == Severity::Warning)
    }

    /// Whether the record may be settled.
    #[must_use]
    pub fn is_settleable(&self) -> bool {
        self.blocking().next().is_none()
    }

    /// One line per finding, for an operator queue.
    pub fn reasons(&self) -> impl Iterator<Item = String> + '_ {
        self.findings.iter().map(ToString::to_string)
    }
}

/// Check a CDR that this workspace did not build.
///
/// ```no_run
/// use emob_cdr::validate;
/// # let cdr: emob_cdr::Cdr = unimplemented!();
///
/// let report = validate(&cdr);
/// if report.is_settleable() {
///     // …rate it
/// } else {
///     for reason in report.reasons() {
///         eprintln!("{reason}");
///     }
/// }
/// ```
#[must_use]
pub fn validate(cdr: &Cdr) -> Report {
    let mut findings = Vec::new();

    if cdr.periods.is_empty() {
        findings.push(Finding::NoPeriods);
    }

    let summed = cdr.periods.iter().map(|p| p.energy).sum::<Energy>();
    if summed != cdr.total_energy {
        findings.push(Finding::DoesNotConserve {
            periods: summed,
            total: cdr.total_energy,
        });
    }

    if cdr.ended_at < cdr.started_at {
        findings.push(Finding::EndsBeforeItStarts);
    } else if cdr.ended_at == cdr.started_at && !cdr.total_energy.is_zero() {
        findings.push(Finding::NoDuration);
    }

    // Period ordering, uniqueness and containment. The window is checked
    // against the *quarter hour* a period belongs to rather than the session
    // instant, because a session starting at 10:07 legitimately reports its
    // first period under the quarter hour beginning 10:00.
    let mut previous: Option<time::OffsetDateTime> = None;
    for period in &cdr.periods {
        if let Some(prev) = previous {
            if period.start < prev {
                findings.push(Finding::PeriodsOutOfOrder {
                    start: period.start,
                });
            } else if period.start == prev {
                findings.push(Finding::DuplicatePeriod {
                    start: period.start,
                });
            }
        }
        let slot_end = period.start + time::Duration::seconds(emob_session::QuarterHour::SECONDS);
        if slot_end <= cdr.started_at || period.start >= cdr.ended_at {
            findings.push(Finding::PeriodOutsideSession {
                start: period.start,
            });
        }
        previous = Some(period.start);
    }

    match &cdr.evidence {
        None => findings.push(Finding::NoEvidence),
        Some(evidence) => {
            let ceiling = cdr.auth_path.strongest_plausible_level();
            if evidence.identification_strength > ceiling {
                findings.push(Finding::AuthStrengthOverstated {
                    ceiling,
                    signed: evidence.identification_strength,
                });
            }
        }
    }

    let interpolated = cdr
        .periods
        .iter()
        .filter(|p| p.provenance == emob_session::Provenance::Interpolated)
        .count();
    if interpolated > 0 {
        findings.push(Finding::Interpolated {
            count: interpolated,
        });
    }

    Report { findings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdr::{CdrKey, ChargingPeriod, EvidenceRef};
    use emob_core::{Direction, PartyId};
    use emob_session::{AuthPath, IdentificationStrength, Provenance};
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use time::macros::datetime;

    fn kwh(s: &str) -> Energy {
        Energy::from_kwh(Decimal::from_str(s).unwrap()).unwrap()
    }

    fn at(minute: i64) -> time::OffsetDateTime {
        datetime!(2026-01-02 10:00 +1) + time::Duration::minutes(minute)
    }

    fn good_cdr() -> Cdr {
        Cdr {
            key: CdrKey {
                party: PartyId::new("DE", "ABC").unwrap(),
                id: "1".parse().unwrap(),
            },
            session_id: "s-1".parse().unwrap(),
            evse_id: "DE*AB7*E840*6487".parse().unwrap(),
            started_at: at(0),
            ended_at: at(30),
            auth_path: AuthPath::Roaming,
            periods: vec![
                ChargingPeriod {
                    start: at(0),
                    energy: kwh("10.000"),
                    provenance: Provenance::Measured,
                },
                ChargingPeriod {
                    start: at(15),
                    energy: kwh("8.000"),
                    provenance: Provenance::Measured,
                },
            ],
            total_energy: kwh("18.000"),
            direction: Direction::Import,
            evidence: Some(EvidenceRef {
                encoding_method: "OCMF".into(),
                payload_digests: vec![[1u8; 32]],
                identification_strength: IdentificationStrength::Trusted,
            }),
            supersedes: None,
        }
    }

    #[test]
    fn a_good_cdr_passes_clean() {
        let report = validate(&good_cdr());
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert!(report.is_settleable());
    }

    #[test]
    fn a_record_that_does_not_add_up_is_blocked() {
        let mut cdr = good_cdr();
        cdr.total_energy = kwh("20.000");
        let report = validate(&cdr);
        assert!(!report.is_settleable());
        assert!(
            report
                .reasons()
                .any(|r| r.contains("sum to 18.000 kWh but the record claims 20.000 kWh"))
        );
    }

    #[test]
    fn nothing_is_silently_repaired() {
        let mut cdr = good_cdr();
        cdr.total_energy = kwh("20.000");
        let before = cdr.clone();
        let _ = validate(&cdr);
        assert_eq!(cdr, before, "validation never mutates");
    }

    #[test]
    fn every_problem_is_reported_not_just_the_first() {
        let mut cdr = good_cdr();
        cdr.total_energy = kwh("20.000");
        cdr.ended_at = at(-5);
        cdr.evidence = None;

        let report = validate(&cdr);
        assert!(report.findings.len() >= 3, "{:?}", report.findings);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::DoesNotConserve { .. }))
        );
        assert!(report.findings.contains(&Finding::EndsBeforeItStarts));
        assert!(report.findings.contains(&Finding::NoEvidence));
    }

    #[test]
    fn missing_evidence_is_a_warning_not_a_block() {
        // Blocking here would make this crate refuse lawful settlement outside
        // Germany; the billing layer knows which regime applies.
        let mut cdr = good_cdr();
        cdr.evidence = None;
        let report = validate(&cdr);
        assert!(report.is_settleable());
        assert_eq!(report.warnings().count(), 1);
    }

    #[test]
    fn an_overstated_authorisation_blocks() {
        let mut cdr = good_cdr();
        cdr.auth_path = AuthPath::AutoCharge; // hearsay at best
        if let Some(e) = cdr.evidence.as_mut() {
            e.identification_strength = IdentificationStrength::Secure;
        }
        let report = validate(&cdr);
        assert!(!report.is_settleable());
        assert!(
            report
                .reasons()
                .any(|r| r.contains("claims secure identification"))
        );
    }

    #[test]
    fn out_of_order_periods_are_caught() {
        let mut cdr = good_cdr();
        cdr.periods.swap(0, 1);
        let report = validate(&cdr);
        assert!(!report.is_settleable());
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::PeriodsOutOfOrder { .. }))
        );
    }

    #[test]
    fn duplicate_periods_are_caught() {
        let mut cdr = good_cdr();
        cdr.periods[1].start = cdr.periods[0].start;
        cdr.periods[1].energy = kwh("8.000");
        let report = validate(&cdr);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::DuplicatePeriod { .. }))
        );
    }

    #[test]
    fn a_period_outside_the_window_is_caught() {
        let mut cdr = good_cdr();
        cdr.periods.push(ChargingPeriod {
            start: at(120),
            energy: kwh("0"),
            provenance: Provenance::Measured,
        });
        let report = validate(&cdr);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::PeriodOutsideSession { .. }))
        );
    }

    #[test]
    fn a_session_starting_mid_slot_is_not_flagged_as_outside_its_window() {
        // The first period is reported under the quarter hour beginning 10:00
        // even though the session began at 10:07. That is correct, not a fault.
        let mut cdr = good_cdr();
        cdr.started_at = at(7);
        cdr.ended_at = at(23);
        let report = validate(&cdr);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::PeriodOutsideSession { .. })),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn interpolation_is_reported_as_a_warning() {
        let mut cdr = good_cdr();
        cdr.periods[0].provenance = Provenance::Interpolated;
        let report = validate(&cdr);
        assert!(report.is_settleable());
        assert!(
            report
                .findings
                .contains(&Finding::Interpolated { count: 1 })
        );
    }

    #[test]
    fn an_empty_record_is_blocked() {
        let mut cdr = good_cdr();
        cdr.periods.clear();
        cdr.total_energy = Energy::ZERO;
        let report = validate(&cdr);
        assert!(!report.is_settleable());
        assert!(report.findings.contains(&Finding::NoPeriods));
    }

    #[test]
    fn energy_without_duration_is_blocked() {
        let mut cdr = good_cdr();
        cdr.ended_at = cdr.started_at;
        let report = validate(&cdr);
        assert!(report.findings.contains(&Finding::NoDuration));
        assert!(!report.is_settleable());
    }
}
