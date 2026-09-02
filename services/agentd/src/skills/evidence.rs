//! Triage over a day's refused sessions.
//!
//! # Why an agent and not a query
//!
//! `emob-eichrecht` already answers, per session, whether the energy may be
//! billed and why not. What it cannot answer is the question an operator
//! actually has at eight in the morning: **which one thing should I fix today.**
//!
//! Four hundred refused sessions are four hundred support tickets or they are
//! one hardware fault, and the difference is whether anybody grouped them. This
//! specialist groups by the **signing component** and by the reason, and ranks
//! by the kilowatt-hours that cannot be billed — because that is the quantity
//! the operator will be asked about and the one `[MessEG §33]` turns on.
//!
//! # It reads findings; it does not re-derive them
//!
//! The input is what `ChainReport::reasons()` produced, not the records. A
//! second implementation of "may this be billed" living in an agent is exactly
//! the drift this workspace refuses everywhere else — and it would be the one
//! implementation nobody tested against a real meter.

use std::collections::BTreeMap;

use agentplane::prelude::*;
use emob_core::Energy;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::advice::{Advice, AtRisk, Proposal};

/// One session the chain refused, as a daemon hands it over.
///
/// A serde type rather than a `ChainReport`, because this is the boundary
/// between the domain and the plane: an agent's input is journaled, and a
/// journal holds JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedSession {
    /// Which session.
    pub session: String,
    /// Where.
    pub evse_id: String,
    /// The signing component the records named, when they named one.
    ///
    /// The field the grouping turns on: a meter in a bad state refuses every
    /// session it signs, and that is one fault rather than many.
    pub signing_component: Option<String>,
    /// The energy the chain would have billed had it held up.
    pub energy_at_risk: Energy,
    /// What the chain said, from `ChainReport::reasons()`.
    pub reasons: Vec<String>,
}

/// The input one run reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusals {
    /// The refused sessions of a period.
    pub sessions: Vec<RefusedSession>,
}

/// The specialist.
#[derive(Debug, Default)]
pub struct EvidenceTriage;

/// The name this specialist is invoked under, and the one the subscription
/// table names.
pub const NAME: &str = "evidence-triage";

#[async_trait]
impl Skill for EvidenceTriage {
    fn descriptor(&self) -> SkillDescriptor {
        SkillDescriptor::new(NAME)
    }

    async fn invoke(
        &self,
        _cx: &mut StepCtx<'_>,
        input: Tainted<Value>,
    ) -> Result<Outcome, SkillError> {
        // The label travels with the answer: an input that arrived from a
        // partner's document stays marked as such through the whole run.
        let parsed: Result<Refusals, _> = serde_json::from_value(input.peek().clone());
        let refusals = match parsed {
            Ok(refusals) => refusals,
            Err(error) => return Ok(Outcome::fail(format!("unreadable input: {error}"))),
        };

        let proposal = triage(&refusals);
        let value =
            serde_json::to_value(proposal).map_err(|error| SkillError::Other(error.to_string()))?;
        Ok(Outcome::done(input.map(|_| value)))
    }
}

/// Group, count and rank. Pure, so it is testable without a runtime — which is
/// the same rule the domain crates keep, one layer out.
#[must_use]
pub fn triage(refusals: &Refusals) -> Proposal {
    let mut by_component: BTreeMap<(String, String), Bucket> = BTreeMap::new();

    for session in &refusals.sessions {
        // The **first** reason, because `ChainReport` lists them in the order it
        // checked and the first is the one that stopped the session. Grouping on
        // the whole list would make two sessions with one shared fault and one
        // different one look like two faults.
        let Some(reason) = session.reasons.first() else {
            continue;
        };
        let component = session
            .signing_component
            .clone()
            .unwrap_or_else(|| "(no signing component named)".to_owned());
        by_component
            .entry((component, reason.clone()))
            .or_default()
            .add(session);
    }

    let advice = by_component
        .into_iter()
        .map(|((component, reason), bucket)| Advice {
            specialist: NAME.to_owned(),
            headline: format!("{component}: {reason}"),
            at_risk: AtRisk::Energy(bucket.energy),
            evidence: bucket.sessions,
            covers: bucket.count,
            suggested: suggestion(&reason, bucket.count),
        })
        .collect();

    Proposal {
        advice,
        considered: refusals.sessions.len(),
    }
    .ranked()
}

#[derive(Default)]
struct Bucket {
    energy: Energy,
    count: usize,
    sessions: Vec<String>,
}

impl Bucket {
    fn add(&mut self, session: &RefusedSession) {
        self.energy += session.energy_at_risk;
        self.count += 1;
        if self.sessions.len() < Proposal::EVIDENCE_SHOWN {
            self.sessions.push(session.session.clone());
        }
    }
}

/// What a human might do about a reason, in the terms the reason is in.
///
/// A lookup rather than a model: these are the findings `emob-eichrecht`
/// produces, and each has one fix. An agent that phrased them freshly each time
/// would be an agent whose advice a runbook cannot be written against.
fn suggestion(reason: &str, count: usize) -> String {
    let many = count > 1;
    if reason.contains("may not be billed") || reason.contains("Substitute") {
        return format!(
            "the meter reported a state that is not billable{}. That is a device fault \
             rather than a dispute: raise it with the station vendor, and the sessions stay \
             unbilled until it is answered [MessEG §33]",
            if many { " across all of these" } else { "" }
        );
    }
    if reason.contains("no key") || reason.contains("registry") {
        return "no key is registered for this signing component at these instants. Check \
                the provisioning run and the type-approval document the key came with — the \
                sessions become billable retrospectively once the binding is there, because \
                nothing about them changed"
            .to_owned();
    }
    if reason.contains("pagination") {
        return "records are missing from the middle of these sessions. That is a transport \
                or a storage question rather than a metrology one: check whether the station \
                is buffering and re-sending, and whether anything downstream is dropping a \
                retry"
            .to_owned();
    }
    if reason.contains("clock") {
        return "the clock behind these readings is not one a duration may be billed against \
                [OCMF Tab. 19]. The energy is unaffected — a per-kWh tariff still bills — so \
                the fix is either the station's time source or the tariff offered at this \
                point"
            .to_owned();
    }
    "read the finding on one of these sessions and check whether the rest share its cause"
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr as _;

    fn kwh(s: &str) -> Energy {
        Energy::from_kwh(Decimal::from_str(s).unwrap()).unwrap()
    }

    fn session(id: &str, component: &str, energy: &str, reason: &str) -> RefusedSession {
        RefusedSession {
            session: id.to_owned(),
            evse_id: "DE*ABC*E00001".to_owned(),
            signing_component: Some(component.to_owned()),
            energy_at_risk: kwh(energy),
            reasons: vec![reason.to_owned()],
        }
    }

    #[test]
    fn four_hundred_tickets_become_one_fault() {
        // The question an operator has at eight in the morning, and the one no
        // single `ChainReport` can answer.
        let mut sessions: Vec<RefusedSession> = (0..380)
            .map(|n| {
                session(
                    &format!("s-{n}"),
                    "BQ27400330016",
                    "10.000",
                    "the meter was in state Substitute at record 2, which may not be billed",
                )
            })
            .collect();
        sessions.extend((0..20).map(|n| {
            session(
                &format!("t-{n}"),
                "BQ99999999999",
                "1.000",
                "pagination jumped from 4 to 6: a record is missing or duplicated",
            )
        }));

        let proposal = triage(&Refusals { sessions });

        assert_eq!(proposal.considered, 400);
        assert_eq!(proposal.advice.len(), 2, "two causes, not four hundred");

        let first = &proposal.advice[0];
        assert!(first.headline.starts_with("BQ27400330016"), "{first:?}");
        assert_eq!(first.covers, 380);
        assert_eq!(first.at_risk, AtRisk::Energy(kwh("3800.000")));
        assert!(
            first.evidence.len() <= Proposal::EVIDENCE_SHOWN,
            "an operator queue that scrolls is one nobody reads"
        );
        assert!(first.suggested.contains("device fault"), "{first:?}");

        // …and the smaller cause is second, because the ranking is by the
        // quantity the operator will be asked about.
        assert_eq!(proposal.advice[1].covers, 20);
    }

    #[test]
    fn one_component_with_two_causes_is_two_findings() {
        // Grouping on the component alone would hide the second fault behind
        // the louder one.
        let proposal = triage(&Refusals {
            sessions: vec![
                session("a", "M1", "5", "the meter was in state Substitute"),
                session("b", "M1", "5", "pagination jumped from 1 to 3"),
            ],
        });
        assert_eq!(proposal.advice.len(), 2);
        assert!(proposal.advice.iter().all(|a| a.covers == 1));
    }

    #[test]
    fn a_session_with_no_reason_is_not_a_refusal() {
        // The input says it was refused and lists nothing. Inventing a cause
        // for it would put a finding in an operator's queue that no chain
        // produced.
        let proposal = triage(&Refusals {
            sessions: vec![RefusedSession {
                session: "s".into(),
                evse_id: "DE*ABC*E00001".into(),
                signing_component: None,
                energy_at_risk: kwh("1"),
                reasons: Vec::new(),
            }],
        });
        assert!(proposal.advice.is_empty());
        assert_eq!(proposal.considered, 1, "and it still says what it read");
    }

    #[test]
    fn a_component_nothing_named_is_still_grouped_rather_than_dropped() {
        let proposal = triage(&Refusals {
            sessions: vec![RefusedSession {
                session: "s".into(),
                evse_id: "DE*ABC*E00001".into(),
                signing_component: None,
                energy_at_risk: kwh("2"),
                reasons: vec!["no key is registered for this component".into()],
            }],
        });
        assert_eq!(proposal.advice.len(), 1);
        assert!(proposal.advice[0].headline.contains("no signing component"));
        assert!(proposal.advice[0].suggested.contains("provisioning"));
    }
}
