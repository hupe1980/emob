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

use emob_core::{Energy, IdentificationStrength};
use emob_tariff::{Dimension, RatingNote};
use rust_decimal::Decimal;

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
    /// The record carries evidence that says its own energy is not billable.
    ///
    /// Worse than [`Self::NoEvidence`], and blocking where that is a warning: a
    /// record with no evidence may be lawful settlement outside Germany, while
    /// one whose evidence is present and **failed** is a claim its own
    /// attachments contradict.
    EnergyNotBillable,
    /// Some periods were interpolated rather than measured.
    Interpolated {
        /// How many.
        count: usize,
    },
    /// The record has energy but no duration.
    NoDuration,
    /// A period moved energy while claiming the session was not charging.
    ///
    /// The two fields are a claim about one interval, and they can be made to
    /// disagree — by a partner's exporter, or by a translation that inferred
    /// `charging` from something other than the session. It matters because
    /// `[AFIR Art. 5(4)]` prices the two differently: an occupancy fee is a
    /// price for *not* charging, so a period like this is billed at the wrong
    /// rate whichever field the reader believes.
    EnergyWhileNotCharging {
        /// Which period.
        start: time::OffsetDateTime,
        /// What it claims to have moved.
        energy: Energy,
    },
    /// The evidence claims a stronger identification than the authorisation
    /// path supports.
    AuthStrengthOverstated {
        /// What the path can support.
        ceiling: IdentificationStrength,
        /// What the evidence claims.
        signed: IdentificationStrength,
    },
    /// The price was computed for a different amount of energy than the record
    /// claims.
    ///
    /// Only raised when the price actually **charges** for energy. A tariff
    /// with no energy component charges nothing per kWh and prices no
    /// kilowatt-hours, which is not a mismatch — see [`Self::EnergyNotPriced`].
    CostEnergyMismatch {
        /// The energy the price was computed for.
        priced: Decimal,
        /// The energy the record claims.
        total: Energy,
    },
    /// The record moved energy and its price charges nothing per kWh.
    ///
    /// Lawful below 50 kW: `[AFIR Art. 5(4)]` only requires the ad-hoc price to
    /// be based on a price per kWh at 50 kW and above, and a per-minute tariff
    /// on a 22 kW post is an ordinary product. So this is a warning, not a
    /// breach — but it is also the shape a **dropped energy line** takes, and
    /// the two are indistinguishable from the record alone, so a receiving
    /// party is told rather than left to notice.
    EnergyNotPriced {
        /// The energy the record claims.
        total: Energy,
    },
    /// The record claims one direction and its evidence measured the other.
    DirectionMismatch {
        /// What the record claims.
        claimed: emob_core::Direction,
        /// What the signed register says.
        signed: emob_core::Direction,
    },
    /// The record is priced for time, and its own evidence says the duration
    /// may not be billed.
    DurationNotBillable {
        /// Which time dimension the price charges for.
        dimension: Dimension,
    },
    /// The record has been rated, and the rating had something to report.
    ///
    /// A note is not a fault — a block rounding is lawful — but it is a term of
    /// the price that the receiving party has to see rather than discover.
    RatingNote {
        /// What the rating said.
        note: String,
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
            | Self::EnergyNotBillable
            | Self::EnergyWhileNotCharging { .. }
            | Self::AuthStrengthOverstated { .. }
            // Money that was computed for a different quantity than the record
            // states is the settlement fault that costs the most to unwind.
            | Self::CostEnergyMismatch { .. }
            // …and a duration priced off a clock the signed record does not
            // vouch for is a number the payer cannot be asked to accept.
            | Self::DurationNotBillable { .. }
            // Import billed as export is the fault that reverses the sign of a
            // settlement, and `[A6 §IV.1]` will not accept either side of it.
            | Self::DirectionMismatch { .. } => Severity::Blocking,

            // Missing evidence is blocking for a German energy invoice
            // `[MessEG §33]` and merely notable elsewhere, so the *finding* is
            // a warning and the decision belongs to the billing layer that
            // knows which regime applies. Reporting it as blocking here would
            // make this crate refuse perfectly lawful settlement outside
            // Germany.
            // A tariff that charges nothing per kWh is lawful below 50 kW, so
            // this cannot block; it is reported because the same record shape
            // is what a dropped energy line looks like.
            Self::NoEvidence
            | Self::Interpolated { .. }
            | Self::EnergyNotPriced { .. }
            | Self::RatingNote { .. } => Severity::Warning,
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
            Self::EnergyNotBillable => write!(
                f,
                "the evidence attached to this record says its own energy is not billable: a claim its own attachments contradict"
            ),
            Self::Interpolated { count } => write!(
                f,
                "{count} period(s) were interpolated between readings rather than measured"
            ),
            Self::NoDuration => write!(f, "the record has energy but no duration"),
            Self::EnergyWhileNotCharging { start, energy } => write!(
                f,
                "the period at {start} moved {energy} while claiming the session was not charging: an occupancy fee prices exactly the time it was not"
            ),
            Self::AuthStrengthOverstated { ceiling, signed } => write!(
                f,
                "the signed record claims {signed} identification but the authorisation path supports at most {ceiling}"
            ),
            Self::CostEnergyMismatch { priced, total } => write!(
                f,
                "the price was computed for {priced} kWh but the record claims {total}"
            ),
            Self::EnergyNotPriced { total } => write!(
                f,
                "the record moved {total} and its price charges nothing per kWh: lawful below 50 kW [AFIR Art. 5(4)], and also what a dropped energy line looks like"
            ),
            Self::DurationNotBillable { dimension } => write!(
                f,
                "the price charges for {dimension:?} but the signed records do not support billing a duration"
            ),
            Self::DirectionMismatch { claimed, signed } => write!(
                f,
                "the record claims {claimed} but the signed register measured {signed}: import and export never net"
            ),
            Self::RatingNote { note } => write!(f, "the rating reports: {note}"),
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

    check_periods(cdr, &mut findings);

    match &cdr.evidence {
        None => findings.push(Finding::NoEvidence),
        Some(evidence) => {
            if !evidence.energy_billable {
                findings.push(Finding::EnergyNotBillable);
            }
            let ceiling = cdr.auth_path.strongest_plausible_level();
            if evidence.identification_strength > ceiling {
                findings.push(Finding::AuthStrengthOverstated {
                    ceiling,
                    signed: evidence.identification_strength,
                });
            }
            if let Some(signed) = evidence.direction
                && signed != cdr.direction
            {
                findings.push(Finding::DirectionMismatch {
                    claimed: cdr.direction,
                    signed,
                });
            }
        }
    }

    if let Some(cost) = &cdr.cost {
        // The price and the quantity have to be about the same session — but
        // only where the price is *about* energy at all. `quantity_for` sums
        // the lines of a dimension and returns zero for a dimension with none,
        // so comparing it unconditionally read "this tariff charges nothing per
        // kWh" as "this price was computed for 0 kWh" and refused every lawful
        // per-minute tariff below 50 kW as a blocking arithmetic fault.
        //
        // A block rounding legitimately bills more than was delivered, and says
        // so in a note, so it is not a mismatch — anything else is.
        match cost.rated.amount_for(Dimension::Energy) {
            Some(_) => {
                let priced = cost.rated.quantity_for(Dimension::Energy);
                let rounded_up = cost
                    .rated
                    .notes
                    .iter()
                    .any(|n| matches!(n, RatingNote::RoundedToBlock { .. }));
                if priced != cdr.total_energy.kwh()
                    && !(rounded_up && priced > cdr.total_energy.kwh())
                {
                    findings.push(Finding::CostEnergyMismatch {
                        priced,
                        total: cdr.total_energy,
                    });
                }
            }
            None if !cdr.total_energy.is_zero() => {
                findings.push(Finding::EnergyNotPriced {
                    total: cdr.total_energy,
                });
            }
            None => {}
        }
        // The same gate the builder applies, re-applied to a record somebody
        // else built. A partner that prices a duration off an unsynchronised
        // clock has produced a number this side cannot defend either.
        if let Some(evidence) = &cdr.evidence
            && !evidence.duration_billable
        {
            for dimension in [Dimension::Time, Dimension::ParkingTime] {
                if cost.rated.amount_for(dimension).is_some() {
                    findings.push(Finding::DurationNotBillable { dimension });
                }
            }
        }

        for note in &cost.rated.notes {
            findings.push(Finding::RatingNote {
                note: note.to_string(),
            });
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

/// The periods, checked against each other and against the session window.
///
/// A period's window is the part of its settlement slot the meter series
/// actually covered, so it must sit inside the session window exactly — no
/// clamping, no allowance — and it must not contradict its own `charging` flag.
fn check_periods(cdr: &Cdr, findings: &mut Vec<Finding>) {
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
        if period.start < cdr.started_at || period.end > cdr.ended_at || period.end < period.start {
            findings.push(Finding::PeriodOutsideSession {
                start: period.start,
            });
        }
        if !period.charging && !period.energy.is_zero() {
            findings.push(Finding::EnergyWhileNotCharging {
                start: period.start,
                energy: period.energy,
            });
        }
        previous = Some(period.start);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdr::{CdrKey, ChargingPeriod, Cost, EvidenceRef};
    use emob_core::{Currency, Direction, IdentificationStrength, PartyId};
    use emob_session::{AuthPath, Provenance, QuarterHour};
    use emob_tariff::{PriceComponent, Tariff, TariffKind};
    use std::str::FromStr;
    use time::macros::datetime;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn kwh(s: &str) -> Energy {
        Energy::from_kwh(dec(s)).unwrap()
    }

    fn at(minute: i64) -> time::OffsetDateTime {
        datetime!(2026-01-02 10:00 +1) + time::Duration::minutes(minute)
    }

    fn period(from: i64, to: i64, energy: &str) -> ChargingPeriod {
        ChargingPeriod {
            quarter_hour: QuarterHour::containing(at(from)),
            start: at(from),
            end: at(to),
            energy: kwh(energy),
            charging: true,
            provenance: Provenance::Measured,
        }
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
            periods: vec![period(0, 15, "10.000"), period(15, 30, "8.000")],
            total_energy: kwh("18.000"),
            direction: Direction::Import,
            evidence: Some(EvidenceRef {
                encoding_method: "OCMF".into(),
                payload_digests: vec![[1u8; 32]],
                identification_strength: IdentificationStrength::Trusted,
                energy_billable: true,
                duration_billable: true,
                direction: Some(Direction::Import),
            }),
            cost: None,
            supersedes: None,
        }
    }

    fn rated(cdr: &Cdr, tariff: &Tariff) -> Cost {
        Cost {
            tariff_id: tariff.id.clone(),
            tariff_fingerprint: tariff.fingerprint(),
            rated: emob_tariff::rate(tariff, &cdr.chargeable().unwrap()),
        }
    }

    fn energy_tariff(price: &str) -> Tariff {
        Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            vec![PriceComponent::new(
                emob_tariff::Dimension::Energy,
                dec(price),
            )],
        )
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
    fn evidence_that_contradicts_its_own_record_blocks_where_absence_warns() {
        // A record with no evidence may be lawful settlement outside Germany.
        // One whose evidence is present and *failed* is a claim its own
        // attachments contradict, and no regime accepts that.
        let mut cdr = good_cdr();
        cdr.evidence.as_mut().unwrap().energy_billable = false;
        let report = validate(&cdr);
        assert!(!report.is_settleable());
        assert!(report.findings.contains(&Finding::EnergyNotBillable));
        assert!(
            report
                .reasons()
                .any(|r| r.contains("its own attachments contradict"))
        );
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
        cdr.periods[1].end = cdr.periods[0].end;
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
        cdr.periods.push(period(120, 135, "0"));
        let report = validate(&cdr);
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::PeriodOutsideSession { .. }))
        );
    }

    #[test]
    fn a_session_starting_mid_slot_reports_the_slot_and_the_window_separately() {
        // The first period belongs to the quarter hour beginning 10:00 and runs
        // from 10:07, because that is when the session began. Both facts are
        // true; a record that states only the first has a period starting
        // before its own session.
        let cdr = Cdr {
            started_at: at(7),
            ended_at: at(23),
            periods: vec![
                ChargingPeriod {
                    quarter_hour: QuarterHour::containing(at(0)),
                    start: at(7),
                    end: at(15),
                    energy: kwh("4.000"),
                    charging: true,
                    provenance: Provenance::Interpolated,
                },
                ChargingPeriod {
                    quarter_hour: QuarterHour::containing(at(15)),
                    start: at(15),
                    end: at(23),
                    energy: kwh("4.000"),
                    charging: true,
                    provenance: Provenance::Interpolated,
                },
            ],
            total_energy: kwh("8.000"),
            ..good_cdr()
        };

        let report = validate(&cdr);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::PeriodOutsideSession { .. })),
            "{:?}",
            report.findings
        );
        assert_eq!(cdr.periods[0].quarter_hour.start(), at(0));
        assert!(report.is_settleable());
    }

    #[test]
    fn a_price_computed_for_a_different_quantity_blocks_settlement() {
        // The settlement fault that costs the most to unwind: euros that were
        // computed from a session other than the one the record describes.
        let mut cdr = good_cdr();
        cdr.cost = Some(rated(&cdr, &energy_tariff("0.49")));
        assert!(validate(&cdr).is_settleable());

        // The partner restates the energy without re-rating.
        cdr.total_energy = kwh("20.000");
        cdr.periods[1].energy = kwh("10.000");
        let report = validate(&cdr);
        assert!(!report.is_settleable());
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::CostEnergyMismatch { .. })),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn a_lawful_per_minute_tariff_settles_and_says_it_priced_no_energy() {
        // `quantity_for` sums the lines of a dimension and returns zero for a
        // dimension with none, so comparing it unconditionally read "this
        // tariff charges nothing per kWh" as "this price was computed for
        // 0 kWh" — and refused every per-minute tariff below 50 kW, which
        // `[AFIR Art. 5(4)]` permits, as a blocking arithmetic fault.
        let mut cdr = good_cdr();
        let by_the_minute = Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            vec![PriceComponent::new(
                emob_tariff::Dimension::Time,
                dec("6.00"),
            )],
        );
        cdr.cost = Some(rated(&cdr, &by_the_minute));

        let report = validate(&cdr);
        assert!(
            report.is_settleable(),
            "a 22 kW post priced per minute is an ordinary product: {:?}",
            report.blocking().collect::<Vec<_>>()
        );
        // …and the fact is still reported, because a dropped energy line looks
        // exactly like this and the record alone cannot tell them apart.
        assert!(
            report
                .warnings()
                .any(|f| matches!(f, Finding::EnergyNotPriced { .. })),
            "{:?}",
            report.findings
        );
        assert!(
            report
                .reasons()
                .any(|r| r.contains("charges nothing per kWh"))
        );
    }

    #[test]
    fn a_partner_claiming_the_wrong_direction_is_blocked() {
        let mut cdr = good_cdr();
        cdr.evidence.as_mut().unwrap().direction = Some(Direction::Export);
        let report = validate(&cdr);
        assert!(!report.is_settleable());
        assert!(report.reasons().any(|r| r.contains("never net")));
    }

    #[test]
    fn a_partner_pricing_a_duration_off_an_undefendable_clock_is_blocked() {
        let mut cdr = good_cdr();
        cdr.evidence.as_mut().unwrap().duration_billable = false;

        let occupancy = Tariff::simple(
            "t".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            vec![
                PriceComponent::new(emob_tariff::Dimension::Energy, dec("0.49")),
                PriceComponent::new(emob_tariff::Dimension::Time, dec("6.00")),
            ],
        );
        cdr.cost = Some(rated(&cdr, &occupancy));

        let report = validate(&cdr);
        assert!(!report.is_settleable());
        assert!(
            report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::DurationNotBillable { .. })),
            "{:?}",
            report.findings
        );

        // A per-kWh price on the same record settles: the register is fine.
        cdr.cost = Some(rated(&cdr, &energy_tariff("0.49")));
        assert!(validate(&cdr).is_settleable());
    }

    #[test]
    fn a_rating_note_travels_as_a_warning_rather_than_a_block() {
        let mut cdr = good_cdr();
        let mut tariff = energy_tariff("0.49");
        tariff.min_price = Some(dec("50.00"));
        cdr.cost = Some(rated(&cdr, &tariff));

        let report = validate(&cdr);
        assert!(report.is_settleable(), "a minimum charge is lawful");
        assert!(
            report.reasons().any(|r| r.contains("minimum")),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn block_rounding_bills_more_than_was_delivered_without_being_a_mismatch() {
        let mut cdr = good_cdr();
        let mut tariff = energy_tariff("0.49");
        tariff.elements[0].components[0].step_size = 1_000_000; // whole MWh
        cdr.cost = Some(rated(&cdr, &tariff));

        let report = validate(&cdr);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| matches!(f, Finding::CostEnergyMismatch { .. })),
            "rounding up is lawful and says so in its own note: {:?}",
            report.findings
        );
        assert!(report.reasons().any(|r| r.contains("rounded up")));
    }

    #[test]
    fn a_period_that_moved_energy_while_not_charging_contradicts_itself() {
        // The two fields are a claim about one interval. A partner whose
        // exporter marks a period as occupancy while reporting energy across it
        // has produced a record that is billed at the wrong rate whichever
        // field the reader believes.
        let mut cdr = good_cdr();
        cdr.periods[1].charging = false;

        let report = validate(&cdr);
        assert!(!report.is_settleable());
        assert!(
            report.reasons().any(|r| r.contains("occupancy fee prices")),
            "{:?}",
            report.findings
        );

        // Occupancy that moved nothing is the ordinary case, and passes.
        cdr.periods[1].energy = Energy::ZERO;
        cdr.total_energy = kwh("10.000");
        assert!(validate(&cdr).is_settleable());
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
