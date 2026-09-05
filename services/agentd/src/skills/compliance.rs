//! The estate against the obligation calendar, and the breaches that have not
//! started yet.
//!
//! # The rule is `emob-core`'s; the sweep is the agent's
//!
//! `emob_core::obligation::assess` judges **one** charge point against the
//! **47 duties** on one date. It is the only implementation of those rules and
//! this specialist does not have a second one — it calls it, once per point.
//!
//! What it adds is the two things a single assessment cannot say.
//!
//! **Which duty, across how many points.** An operator with four hundred posts
//! does not have four hundred compliance questions; it has the handful of duties
//! its estate fails and the list of posts under each. Forty-seven findings per point
//! is a report nobody reads.
//!
//! **And the breaches that have not started.** Every duty carries the date it
//! begins binding, so one that is [`Status::NotYetInForce`] today is judged *at
//! its own commencement date* against the estate as it stands. A point that will
//! fail `[DA-656 Anh. 2.1.2]` on 01.01.2027 is a firmware programme now and an
//! enforcement letter later, and that forecast is the only compliance advice
//! arriving before the breach rather than after it.
//!
//! # What it does not do
//!
//! It does not decide whether a fact about a point is true. Every field of a
//! [`ChargePointProfile`] is somebody else's to establish — an inventory, a type
//! approval, a contract with a site host — and a specialist that inferred one
//! would answer a compliance question with a guess. What leaves is [`Advice`],
//! which nothing consumes.

use agentplane::prelude::*;
use emob_core::obligation::{Consequence, Obligation, ObligationId, Status, assess};
use emob_core::station::ChargePointProfile;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::advice::{Advice, AtRisk, Proposal};

/// The input one run reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Estate {
    /// The day the question is asked about.
    ///
    /// An argument rather than a clock, for the reason every instant in this
    /// workspace is one: a sweep that read `today` could not be replayed, and
    /// the journal's whole value is that a run in March re-executes to the same
    /// answer in September.
    #[serde(with = "emob_core::wire::date")]
    pub on: time::Date,
    /// The estate, as the inventory states it.
    pub points: Vec<ChargePointProfile>,
    /// How far ahead to forecast, in days.
    ///
    /// `None` forecasts every duty in the calendar that has not started,
    /// however distant. A horizon is what an operator with a budget cycle
    /// wants; no horizon is what somebody planning a hardware refresh wants,
    /// and neither is a default the daemon may pick.
    #[serde(default)]
    pub horizon_days: Option<u16>,
}

/// The specialist.
#[derive(Debug, Default)]
pub struct ComplianceSweep;

/// The name this specialist is invoked under.
pub const NAME: &str = "compliance-sweep";

#[async_trait]
impl Skill for ComplianceSweep {
    fn descriptor(&self) -> SkillDescriptor {
        SkillDescriptor::new(NAME)
    }

    async fn invoke(
        &self,
        _cx: &mut StepCtx<'_>,
        input: Tainted<Value>,
    ) -> Result<Outcome, SkillError> {
        let parsed: Result<Estate, _> = serde_json::from_value(input.peek().clone());
        let estate = match parsed {
            Ok(estate) => estate,
            Err(error) => return Ok(Outcome::fail(format!("unreadable input: {error}"))),
        };

        let proposal = sweep(&estate);
        let value =
            serde_json::to_value(proposal).map_err(|error| SkillError::Other(error.to_string()))?;
        Ok(Outcome::done(input.map(|_| value)))
    }
}

/// Judge every point against the calendar, and group by duty.
///
/// Pure, so it is testable without a runtime.
#[must_use]
pub fn sweep(estate: &Estate) -> Proposal {
    // Keyed by the duty, because the duty is the unit of work: one firmware
    // programme, one contract clause, one retrofit. Grouping by point instead
    // produces four hundred findings that are the same finding.
    let mut failing: BTreeMap<ObligationId, (Obligation, Vec<String>)> = BTreeMap::new();
    let mut forecast: BTreeMap<ObligationId, (Obligation, Vec<String>)> = BTreeMap::new();

    for point in &estate.points {
        let subject = point.evse_id.to_string();
        for finding in assess(point, estate.on).findings {
            match finding.status {
                Status::Failing => failing
                    .entry(finding.obligation.id)
                    .or_insert_with(|| (finding.obligation, Vec::new()))
                    .1
                    .push(subject.clone()),
                Status::NotYetInForce => {
                    let starts = finding.obligation.applies_from;
                    if !within(estate, starts) {
                        continue;
                    }
                    // The forecast: judge the *same* estate on the day the duty
                    // begins binding. Everything else about the point is held
                    // still, which is the honest question — "if nothing
                    // changes, is this a breach on the day it starts" — and the
                    // only one the data supports. A point that will have been
                    // renovated by then is a plan the inventory does not carry.
                    if status_on(point, finding.obligation.id, starts) == Some(Status::Failing) {
                        forecast
                            .entry(finding.obligation.id)
                            .or_insert_with(|| (finding.obligation, Vec::new()))
                            .1
                            .push(subject.clone());
                    }
                }
                Status::Satisfied
                | Status::NotApplicable
                | Status::NoLongerInForce
                | Status::DifferentScope => {}
            }
        }
    }

    let advice = failing
        .into_values()
        .map(|(obligation, points)| advise(&obligation, &points, When::Now))
        .chain(
            forecast
                .into_values()
                .map(|(obligation, points)| advise(&obligation, &points, When::From)),
        )
        .collect();

    Proposal {
        advice,
        considered: estate.points.len(),
    }
    .ranked()
}

/// Whether a commencement date is inside the estate's forecast horizon.
fn within(estate: &Estate, starts: time::Date) -> bool {
    estate.horizon_days.is_none_or(|days| {
        starts
            <= estate
                .on
                .saturating_add(time::Duration::days(i64::from(days)))
    })
}

/// One duty's status for one point on one date.
fn status_on(point: &ChargePointProfile, id: ObligationId, on: time::Date) -> Option<Status> {
    assess(point, on).status_of(id)
}

/// Whether a duty already binds or is still ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When {
    /// In force today, and failing.
    Now,
    /// Not yet in force, and this estate would fail it on the day it starts.
    From,
}

/// The headline, in the words the consequence actually has.
///
/// A forgone entitlement is not a breach and must not be worded as one: an
/// operator that declines the greenhouse-gas quota has broken no law, and a
/// queue that says "fail" about it is a queue that teaches its reader to
/// discount the ones that matter (D219). The calendar carries the distinction
/// and this is the one place it reaches a human.
fn advise(obligation: &Obligation, points: &[String], when: When) -> Advice {
    let n = points.len();
    let headline = match (obligation.consequence, when) {
        (Consequence::Breach, When::Now) => format!(
            "{n} point(s) fail `{}` {}: {}",
            obligation.id, obligation.citation, obligation.title
        ),
        (Consequence::Breach, When::From) => format!(
            "{n} point(s) will fail `{}` {} from {}: {}",
            obligation.id, obligation.citation, obligation.applies_from, obligation.title
        ),
        (Consequence::ForgoneEntitlement, When::Now) => format!(
            "{n} point(s) forgo an entitlement — lawful, and worth money: `{}` {}: {}",
            obligation.id, obligation.citation, obligation.title
        ),
        (Consequence::ForgoneEntitlement, When::From) => format!(
            "{n} point(s) will forgo an entitlement from {} — lawful, and worth money: `{}` {}: {}",
            obligation.applies_from, obligation.id, obligation.citation, obligation.title
        ),
    };
    Advice {
        specialist: NAME.to_owned(),
        headline,
        at_risk: AtRisk::Count {
            n: points.len(),
            of: "charge points".to_owned(),
        },
        evidence: points
            .iter()
            .take(Proposal::EVIDENCE_SHOWN)
            .cloned()
            .collect(),
        covers: points.len(),
        // The calendar's own remedy, never a second wording of it. A specialist
        // that paraphrased would be a second statement of what the article asks
        // for, which is the drift the whole workspace is arranged to prevent.
        suggested: obligation.remedy.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_core::station::AdHocPayment;
    use rust_decimal::Decimal;
    use time::macros::date;

    /// A fast public post that takes cards and meets everything the calendar
    /// asks of it on the date in question.
    fn compliant(id: &str, commissioned: time::Date) -> ChargePointProfile {
        let mut point = ChargePointProfile::bare(id.parse().expect("an EVSE id"), commissioned);
        point.rated_power_kw = Decimal::from(150);
        point.ad_hoc_payment = AdHocPayment::CardReader;
        point.digitally_connected = true;
        point.smart_recharging_capable = true;
        point.fixed_cable = true;
        point.meets_technical_requirements = true;
        point.data = emob_core::station::DataPublication {
            static_data: true,
            dynamic_data: true,
            open_api: true,
            datex2: true,
        };
        point.price_transparency = emob_core::station::PriceTransparency {
            energy_based: true,
            shown_at_station: true,
            components_in_prescribed_order: true,
            arbeitspreis: emob_core::ArbeitspreisIndication::PointDisplay,
            web_based_ad_hoc: false,
            arbeitspreis_stated_before_start: false,
            additional_prices: emob_core::AdditionalPrices::None,
        };
        point.connectors = vec![emob_core::ConnectorType::Iec62196T2Combo];
        point.current_type = emob_core::CurrentType::Dc;
        point.registration.technical_documentation_available = true;
        point.registration.commissioning_notified_on = Some(commissioned);
        point.metering.bills_by_energy = false;
        // The greenhouse-gas quota is an *entitlement*, not a duty, so an estate
        // that declines it is lawful — but a fixture that declines it would put
        // an entitlement finding in every test here and hide the duty each one
        // is about. Claimed, so the tests below are about what they say.
        point.quota = emob_core::station::QuotaPosture {
            publication: emob_core::station::RegisterPublication::Published,
            conformity_declared: true,
            operator_code_assigned: true,
            further_identifiers: emob_core::station::FurtherIdentifiers::NoneAnnounced,
        };
        point
    }

    fn estate(points: Vec<ChargePointProfile>, on: time::Date) -> Estate {
        Estate {
            on,
            points,
            horizon_days: None,
        }
    }

    #[test]
    fn one_duty_failed_by_four_hundred_points_is_one_finding() {
        // The question an operator has is never "what is wrong with post 217".
        // It is "which duties does my estate fail, and how many posts under
        // each" — one firmware programme, one contract clause, one retrofit.
        // Forty-seven findings per point is a report nobody reads.
        let mut points = Vec::new();
        for n in 1..=400 {
            let mut point = compliant(&format!("DE*ABC*E{n:05}"), date!(2025 - 06 - 01));
            point.digitally_connected = false;
            points.push(point);
        }

        let proposal = sweep(&estate(points, date!(2026 - 09 - 01)));
        assert_eq!(proposal.considered, 400);
        assert_eq!(proposal.advice.len(), 1, "{:?}", proposal.advice);

        let advice = &proposal.advice[0];
        assert_eq!(advice.covers, 400);
        assert_eq!(
            advice.evidence.len(),
            Proposal::EVIDENCE_SHOWN,
            "a finding covering four hundred posts names a few and counts the rest"
        );
        assert!(advice.headline.contains("[AFIR Art. 5(7)]"), "{advice:?}");
        assert!(
            advice.suggested.contains("connect the point to a CSMS"),
            "the calendar's own remedy, never a second wording of it: {advice:?}"
        );
    }

    #[test]
    fn a_duty_that_has_not_started_is_advice_before_the_breach_and_not_after_it() {
        // The half the dated calendar exists for, and the half nothing else in
        // the workspace uses. `[DA-656 Anh. 2.1.2]` binds public points
        // installed or renovated from 01.01.2027; a point renovated in 2027
        // that speaks only -2 is compliant today and in breach on new year's
        // day, and only a calendar that carries the date can say so now.
        let mut point = compliant("DE*ABC*E00001", date!(2025 - 06 - 01));
        point.renovated_on = Some(date!(2027 - 03 - 01));
        point.v2g.iso15118_2 = true;
        point.v2g.iso15118_20 = false;

        let today = sweep(&estate(vec![point.clone()], date!(2026 - 09 - 01)));

        // Nothing is failing today…
        assert!(
            !today
                .advice
                .iter()
                .any(|a| a.headline.starts_with("1 point(s) fail")),
            "{:?}",
            today.advice
        );
        // …and the duty that has not started is named with the date it does.
        let forecast = today
            .advice
            .iter()
            .find(|a| a.headline.contains("[DA-656 Anh. 2.1.2]"))
            .expect("the 2027 generation is forecast");
        assert!(forecast.headline.contains("will fail"), "{forecast:?}");
        assert!(forecast.headline.contains("2027-01-01"), "{forecast:?}");

        // And it is a forecast rather than a guess: give the point the firmware
        // and the same sweep says nothing about it.
        point.v2g.iso15118_20 = true;
        let fixed = sweep(&estate(vec![point], date!(2026 - 09 - 01)));
        assert!(
            !fixed
                .advice
                .iter()
                .any(|a| a.headline.contains("[DA-656 Anh. 2.1.2]")),
            "{:?}",
            fixed.advice
        );
    }

    #[test]
    fn a_horizon_is_the_callers_and_never_the_daemons() {
        // A budget cycle wants eighteen months; a hardware refresh wants
        // everything. Neither is a default a daemon may pick, so `None` means
        // the whole calendar and a number means that number of days.
        let mut point = compliant("DE*ABC*E00001", date!(2025 - 06 - 01));
        point.renovated_on = Some(date!(2027 - 03 - 01));
        point.v2g.iso15118_2 = true;
        point.v2g.iso15118_20 = false;
        let on = date!(2026 - 09 - 01);

        let unbounded = sweep(&estate(vec![point.clone()], on));
        assert!(!unbounded.advice.is_empty());

        // Thirty days reaches nothing: the duty starts on 01.01.2027.
        let near = sweep(&Estate {
            on,
            points: vec![point],
            horizon_days: Some(30),
        });
        assert!(near.advice.is_empty(), "{:?}", near.advice);
    }

    #[test]
    fn a_forgone_entitlement_is_not_worded_as_a_breach() {
        // The greenhouse-gas quota is money, not law. An operator that declines
        // it has broken nothing, and a queue that says "fail" about it teaches
        // its reader to discount the findings that matter (D219).
        let mut point = compliant("DE*ABC*E00001", date!(2025 - 06 - 01));
        point.quota = emob_core::station::QuotaPosture::default();

        let proposal = sweep(&estate(vec![point], date!(2026 - 09 - 01)));
        assert_eq!(proposal.advice.len(), 1, "{:?}", proposal.advice);
        let advice = &proposal.advice[0];
        assert!(advice.headline.contains("[38k §6(3)]"), "{advice:?}");
        assert!(
            advice.headline.contains("forgo an entitlement"),
            "{advice:?}"
        );
        assert!(
            !advice.headline.contains("fail"),
            "a lawful estate is not failing: {advice:?}"
        );
    }

    #[test]
    fn a_compliant_estate_produces_nothing_and_still_says_what_it_read() {
        // A specialist that had an opinion about a compliant estate is one whose
        // queue stops being read.
        let proposal = sweep(&estate(
            vec![
                compliant("DE*ABC*E00001", date!(2025 - 06 - 01)),
                compliant("DE*ABC*E00002", date!(2025 - 06 - 01)),
            ],
            date!(2026 - 09 - 01),
        ));
        assert!(proposal.advice.is_empty(), "{:?}", proposal.advice);
        assert_eq!(proposal.considered, 2);
    }
}
