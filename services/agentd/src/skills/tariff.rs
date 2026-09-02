//! Review of the tariffs an estate offers, at the powers it offers them at.
//!
//! # The rule is the crate's; the sweep is the agent's
//!
//! `emob_tariff::check_afir` decides whether one tariff is lawful at one power.
//! It is the only implementation of that rule and this specialist does not have
//! a second one — it calls it.
//!
//! What it adds is the sweep. A tariff is not lawful or unlawful on its own:
//! `[AFIR Art. 5(4)]` binds it **at the power the point offers it at**, so the
//! same per-minute tariff is an ordinary product on a 22 kW post and a breach on
//! the 150 kW cabinet beside it. An operator with one tariff and a mixed estate
//! is in breach at some of its points and not others, and nothing in a tariff
//! document says which.
//!
//! So the input is the pairing — every tariff against every rated power it is
//! actually offered at — and the finding names the points.

use agentplane::prelude::*;
use emob_tariff::{Objection, Tariff, check_afir};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::advice::{Advice, AtRisk, Proposal};

/// One tariff, and the points that offer it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Offering {
    /// The tariff, exactly as it rates.
    pub tariff: Tariff,
    /// The points that offer it, and the power each is rated at.
    pub at_points: Vec<Point>,
}

/// A charge point and what it can deliver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// Which point.
    pub evse_id: String,
    /// Its rated power in kW — the figure `[AFIR Art. 5(4)]`'s 50 kW threshold
    /// is read against.
    pub rated_power_kw: Decimal,
}

/// The input one run reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Estate {
    /// Every tariff the estate offers, with the points offering it.
    pub offerings: Vec<Offering>,
}

/// The specialist.
#[derive(Debug, Default)]
pub struct TariffReview;

/// The name this specialist is invoked under.
pub const NAME: &str = "tariff-review";

#[async_trait]
impl Skill for TariffReview {
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

        let proposal = review(&estate);
        let value =
            serde_json::to_value(proposal).map_err(|error| SkillError::Other(error.to_string()))?;
        Ok(Outcome::done(input.map(|_| value)))
    }
}

/// Check every tariff at every power it is offered at, and group by what a
/// regulator would object to.
///
/// Pure, so it is testable without a runtime.
#[must_use]
pub fn review(estate: &Estate) -> Proposal {
    // Keyed by the objection *and* the tariff, because one tariff breaching in
    // two ways is two things to fix and two tariffs breaching the same way is
    // one thing to understand.
    let mut findings: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut considered = 0;

    for offering in &estate.offerings {
        for point in &offering.at_points {
            considered += 1;
            for objection in check_afir(&offering.tariff, point.rated_power_kw)
                .objections
                .iter()
                .filter(|objection| objection.is_breach())
            {
                findings
                    .entry((offering.tariff.id.to_string(), describe(objection)))
                    .or_default()
                    .push(point.evse_id.clone());
            }
        }
    }

    let advice = findings
        .into_iter()
        .map(|((tariff, objection), points)| Advice {
            specialist: NAME.to_owned(),
            headline: format!(
                "`{tariff}` is unlawful at {} point(s): {objection}",
                points.len()
            ),
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
            suggested: remedy(&objection),
        })
        .collect();

    Proposal { advice, considered }.ranked()
}

fn describe(objection: &Objection) -> String {
    objection.to_string()
}

/// What a human might do, in the terms the article is in.
///
/// A lookup rather than a model, for the reason `evidence` gives: these are the
/// objections `emob-tariff` produces, each has one fix, and advice a runbook
/// cannot be written against is advice nobody acts on.
fn remedy(objection: &str) -> String {
    if objection.contains("price per kWh") {
        return "at 50 kW and above the ad-hoc price must be based on a price per kWh \
                [AFIR Art. 5(4)]. Either add an energy component to this tariff, or offer a \
                different tariff at these points — the same document is lawful on the slower \
                posts and this is why the check is per point"
            .to_owned();
    }
    if objection.contains("occupancy fee for not charging") {
        return "the article permits one addition to the kWh price — an occupancy fee for time \
                connected and *not* charging. A price for the charging time itself is not that \
                addition. Move the component to `PARKING_TIME` if that is what was meant"
            .to_owned();
    }
    if objection.contains("per-session fee") {
        return "a per-session fee is listed in the article's third subparagraph, which governs \
                points **below** 50 kW. At and above, the station must show the price per kWh \
                and any occupancy fee and nothing else, so a fee it cannot show is one it \
                cannot charge"
            .to_owned();
    }
    if objection.contains("price per minute") {
        return "this hourly rate has no exact per-minute spelling, and the article asks for a \
                price per minute. Quote an hourly rate divisible by three — the same figure \
                that makes it representable on OCPP 2.1, which quotes time by the minute"
            .to_owned();
    }
    if objection.contains("cannot be evaluated") {
        return "an element restricts on something this build cannot judge, so its price cannot \
                be shown before the session. Either the restriction is one to model or the \
                element is one to remove"
            .to_owned();
    }
    "read the objection against [AFIR Art. 5(4)] and decide whether the tariff or the \
     points it is offered at should change"
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_core::Currency;
    use emob_tariff::{Dimension, PriceComponent, TariffKind};
    use std::str::FromStr as _;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn per_minute() -> Tariff {
        Tariff::simple(
            "by-the-minute".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            vec![PriceComponent::new(Dimension::Time, dec("6.00"))],
        )
    }

    fn point(id: &str, kw: &str) -> Point {
        Point {
            evse_id: id.to_owned(),
            rated_power_kw: dec(kw),
        }
    }

    #[test]
    fn one_tariff_is_lawful_at_some_points_and_not_others() {
        // The thing no tariff document says and no per-tariff check finds: the
        // article binds at the power the point offers.
        let proposal = review(&Estate {
            offerings: vec![Offering {
                tariff: per_minute(),
                at_points: vec![
                    point("DE*ABC*E1", "22"),
                    point("DE*ABC*E2", "22"),
                    point("DE*ABC*E3", "150"),
                    point("DE*ABC*E4", "350"),
                ],
            }],
        });

        assert_eq!(proposal.considered, 4, "every pairing was checked");

        // The tariff breaches twice at a fast point — no price per kWh, and time
        // charged as transfer rather than as occupancy — and **every** finding
        // names the two fast posts and not the two slow ones. That is the whole
        // point of checking per pairing.
        assert!(!proposal.advice.is_empty());
        for finding in &proposal.advice {
            assert_eq!(
                finding.at_risk,
                AtRisk::Count {
                    n: 2,
                    of: "charge points".to_owned()
                },
                "{finding:?}"
            );
            assert_eq!(
                finding.evidence,
                vec!["DE*ABC*E3".to_owned(), "DE*ABC*E4".to_owned()],
                "{finding:?}"
            );
            assert!(finding.headline.contains("by-the-minute"), "{finding:?}");
        }
        assert!(
            proposal
                .advice
                .iter()
                .any(|finding| finding.suggested.contains("per point")),
            "the remedy says why the same tariff is lawful next door"
        );
    }

    #[test]
    fn a_lawful_estate_produces_nothing() {
        let energy = Tariff::simple(
            "ad-hoc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            vec![PriceComponent::new(Dimension::Energy, dec("0.49"))],
        );
        let proposal = review(&Estate {
            offerings: vec![Offering {
                tariff: energy,
                at_points: vec![point("DE*ABC*E1", "22"), point("DE*ABC*E2", "350")],
            }],
        });
        assert!(proposal.advice.is_empty());
        assert_eq!(proposal.considered, 2);
    }

    #[test]
    fn two_breaches_of_one_tariff_are_two_things_to_fix() {
        // A per-minute charging-time price on a fast charger breaches twice —
        // no kWh price, and time charged as transfer rather than occupancy —
        // and folding them into one finding would leave half the fix undone.
        let proposal = review(&Estate {
            offerings: vec![Offering {
                tariff: per_minute(),
                at_points: vec![point("DE*ABC*E1", "150")],
            }],
        });
        assert!(
            proposal.advice.len() >= 2,
            "{:?}",
            proposal
                .advice
                .iter()
                .map(|a| &a.headline)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_objection_that_is_not_a_breach_is_not_a_finding() {
        // `RoundsAgainstTheCustomer` is lawful and worth knowing; it is not
        // something a regulator would stop. Reporting it here would put a
        // permanent entry in a queue that is meant to empty.
        let mut blocks = Tariff::simple(
            "ad-hoc".parse().unwrap(),
            Currency::EUR,
            TariffKind::AdHoc,
            vec![PriceComponent::new(Dimension::Energy, dec("0.49")).with_step_size(1000)],
        );
        blocks.tax_included = emob_tariff::TaxIncluded::Yes;
        let proposal = review(&Estate {
            offerings: vec![Offering {
                tariff: blocks,
                at_points: vec![point("DE*ABC*E1", "150")],
            }],
        });
        assert!(proposal.advice.is_empty(), "{:?}", proposal.advice);
    }
}
