//! What a specialist may say, and the reason it cannot say anything else.
//!
//! # Advisory only, as a property rather than a promise
//!
//! Every agent in this daemon **proposes**. The invariants decide: a chain that
//! does not hold up refuses its energy in `emob-eichrecht`, a record that does
//! not add up is refused in `emob-cdr`, a tariff that breaches
//! `[AFIR Art. 5(4)]` is refused in `emob-tariff`. Nothing an agent says moves
//! money or overrides a guard.
//!
//! Written down, that is a promise somebody has to keep. Two things make it a
//! property instead.
//!
//! **The output type is a leaf.** [`Advice`] carries observations and a
//! recommended action for a human. It has no method that returns a `Cdr`, a
//! `Tariff`, an `Invoice` or a posting, and nothing in this workspace consumes
//! one — so there is no path from an agent's answer into a document. A reviewer
//! checks that by looking at what `Advice` can be turned into, which is nothing.
//!
//! **The principal cannot hold a write capability.** A specialist runs under a
//! principal derived from the operator's by
//! [`emob_service::Principal::attenuate`], which refuses to widen either axis —
//! so [`advisory`] returns a principal restricted to the read capabilities, and
//! a test asserts that no write capability is reachable from it. An agent that
//! wanted to issue an invoice would have to be given a principal that could, and
//! the constructor is the place that would have to change.
//!
//! # Why the advice is ranked by a quantity and not by a score
//!
//! A triage that returns "high, medium, low" has invented a scale. The
//! quantities here are the workspace's own — kilowatt-hours that cannot be
//! billed, money a partner has not accepted — so an operator ranking two
//! findings is comparing the same units they will be asked about.

use emob_core::{Energy, Money};
use emob_service::{Capabilities, Principal, caps};
use serde::{Deserialize, Serialize};

/// What is at stake in a finding, in the workspace's own units.
///
/// Not a severity. A severity is a judgement this daemon is not entitled to
/// make; a quantity is a fact, and two of them can be compared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtRisk {
    /// Energy that cannot be billed.
    Energy(Energy),
    /// Money a counterparty has not accepted.
    Money(Money),
    /// Something countable that is neither — charge points, sessions, stations.
    Count {
        /// How many.
        n: usize,
        /// Of what.
        of: String,
    },
}

impl core::fmt::Display for AtRisk {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Energy(energy) => write!(f, "{energy}"),
            Self::Money(money) => write!(f, "{money}"),
            Self::Count { n, of } => write!(f, "{n} {of}"),
        }
    }
}

/// One thing a specialist noticed, and what it suggests a human do about it.
///
/// A leaf type. See the module documentation: nothing consumes one, and that is
/// the guarantee rather than an omission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Advice {
    /// Which specialist said it.
    pub specialist: String,
    /// One line an operator reads first.
    pub headline: String,
    /// What is at stake.
    pub at_risk: AtRisk,
    /// The evidence: the records, sessions or stations this is about, named so
    /// somebody can go and look.
    ///
    /// Bounded, because a finding covering four hundred sessions is not made
    /// more useful by listing all four hundred — [`Proposal::EVIDENCE_SHOWN`]
    /// is the cut, and [`Advice::covers`] says how many there really were.
    pub evidence: Vec<String>,
    /// How many the finding covers, of which [`Self::evidence`] names some.
    pub covers: usize,
    /// What a human might do. **A suggestion, never an instruction to a
    /// machine**: nothing in this workspace reads this field.
    pub suggested: String,
}

/// Everything a specialist produced in one run.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Proposal {
    /// The advice, most at risk first.
    pub advice: Vec<Advice>,
    /// How many inputs the specialist read.
    pub considered: usize,
}

impl Proposal {
    /// How many named records an [`Advice`] shows before it says "and n more".
    ///
    /// A finding covering four hundred sessions is not made more useful by
    /// listing all four hundred, and an operator queue that scrolls is one
    /// nobody reads.
    pub const EVIDENCE_SHOWN: usize = 5;

    /// Sort the advice so the largest quantity is first, **within a
    /// comparable group**.
    ///
    /// Two different kinds are not ranked against each other, because there is
    /// no exchange rate between a kilowatt-hour and a euro that this daemon is
    /// entitled to invent.
    ///
    /// # …and a kind is not a group
    ///
    /// The same argument goes one level further. €100 and CHF 100 are both
    /// [`AtRisk::Money`], and ordering them by amount invents the exchange rate
    /// the paragraph above refuses. 400 sessions and 5 charge points are both
    /// [`AtRisk::Count`], and ranking them says four hundred sessions matter
    /// eighty times more than five dead posts, which is not a fact.
    ///
    /// So the group is the **unit**: the currency for money, the counted noun
    /// for a count, kilowatt-hours for energy (there is only one). Groups are
    /// ordered among themselves by name so a queue is stable, and each group is
    /// ordered by magnitude (D189).
    #[must_use]
    pub fn ranked(mut self) -> Self {
        self.advice.sort_by(|a, b| {
            kind_order(&a.at_risk)
                .cmp(&kind_order(&b.at_risk))
                .then_with(|| unit_of(&a.at_risk).cmp(&unit_of(&b.at_risk)))
                .then_with(|| compare_within_unit(&b.at_risk, &a.at_risk))
                .then_with(|| a.headline.cmp(&b.headline))
        });
        self
    }

    /// One line per piece of advice, for an operator queue.
    pub fn lines(&self) -> impl Iterator<Item = String> + '_ {
        self.advice.iter().map(|advice| {
            format!(
                "[{}] {} ({} at risk across {}) — {}",
                advice.specialist, advice.headline, advice.at_risk, advice.covers, advice.suggested
            )
        })
    }
}

const fn kind_order(at_risk: &AtRisk) -> u8 {
    match at_risk {
        AtRisk::Money(_) => 0,
        AtRisk::Energy(_) => 1,
        AtRisk::Count { .. } => 2,
    }
}

/// The unit two quantities have to share before their magnitudes mean
/// anything against each other.
fn unit_of(at_risk: &AtRisk) -> String {
    match at_risk {
        // One unit, so every energy is comparable with every other.
        AtRisk::Energy(_) => "kWh".to_owned(),
        AtRisk::Money(money) => money.currency().to_string(),
        AtRisk::Count { of, .. } => of.clone(),
    }
}

/// Compare two quantities **already known to share a unit**.
fn compare_within_unit(a: &AtRisk, b: &AtRisk) -> core::cmp::Ordering {
    match (a, b) {
        (AtRisk::Energy(a), AtRisk::Energy(b)) => a.cmp(b),
        (AtRisk::Money(a), AtRisk::Money(b)) => a.amount().cmp(&b.amount()),
        (AtRisk::Count { n: a, .. }, AtRisk::Count { n: b, .. }) => a.cmp(b),
        _ => core::cmp::Ordering::Equal,
    }
}

/// The capabilities a specialist may hold. Every one of them reads.
///
/// A `const` list rather than a set built at each call site, because the
/// guarantee this daemon makes is about *this* list and a test asserts what is
/// not in it.
pub const ADVISORY: &[&str] = &[
    caps::CDR_READ,
    caps::EVIDENCE_READ,
    caps::TARIFF_READ,
    caps::LOCATION_READ,
    caps::SESSION_READ,
    caps::INVOICE_READ,
];

/// A principal a specialist may run under, derived from the operator's own.
///
/// `None` when the operator does not itself hold every advisory capability —
/// because [`Principal::attenuate`] refuses to widen, and a delegation that
/// quietly granted less than it was asked for is a permission error that
/// surfaces somewhere else entirely.
///
/// This is where "advisory only" stops being a promise. An agent that wanted to
/// issue an invoice would need a principal holding
/// [`emob_service::caps::INVOICE_WRITE`], and the only constructor in this
/// daemon is this one.
#[must_use]
pub fn advisory(operator: &Principal) -> Option<Principal> {
    operator.attenuate(
        Capabilities::of(ADVISORY.iter().copied()),
        operator.scope.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_core::{Currency, Money, PartyId};
    use emob_service::{PartyScope, Role};
    use rust_decimal::Decimal;
    use std::str::FromStr as _;

    fn operator() -> Principal {
        Principal::operator(PartyId::new("DE", "ABC").unwrap(), Role::Cpo)
    }

    fn kwh(s: &str) -> Energy {
        Energy::from_kwh(Decimal::from_str(s).unwrap()).unwrap()
    }

    #[test]
    fn no_specialist_can_hold_a_capability_that_writes() {
        // The advisory-only rule, as a test rather than a paragraph. Every
        // write capability this workspace names is checked, so a new one added
        // to `caps` and forgotten here fails to compile rather than quietly
        // becoming reachable.
        let agent = advisory(&operator()).expect("the operator holds everything");
        for capability in [
            caps::CDR_WRITE,
            caps::TARIFF_WRITE,
            caps::LOCATION_WRITE,
            caps::INVOICE_WRITE,
            caps::TOKEN_AUTHORIZE,
        ] {
            assert!(
                !agent.may(capability),
                "an advisory principal must not hold {capability}"
            );
        }
        // …and it does hold what it needs to read.
        for capability in ADVISORY {
            assert!(agent.may(capability), "{capability}");
        }
    }

    #[test]
    fn an_agent_cannot_be_derived_from_an_operator_that_holds_less() {
        // A delegation that quietly granted less than it was asked for is a
        // permission error that surfaces somewhere else entirely.
        let narrow = Principal::peer(
            PartyId::new("DE", "ABC").unwrap(),
            Role::Cpo,
            Capabilities::of([caps::CDR_READ]),
        );
        assert!(advisory(&narrow).is_none());
    }

    #[test]
    fn an_agent_reaches_no_further_than_the_operator_it_acts_for() {
        let operator = Principal {
            scope: PartyScope::just(&PartyId::new("DE", "ABC").unwrap()),
            ..operator()
        };
        let agent = advisory(&operator).expect("derivable");
        assert!(agent.may_reach(&PartyId::new("DE", "ABC").unwrap()));
        assert!(!agent.may_reach(&PartyId::new("DE", "XYZ").unwrap()));
    }

    #[test]
    fn advice_is_ranked_by_a_quantity_and_never_across_kinds() {
        // Two findings in different units are grouped rather than compared,
        // because there is no exchange rate between a kilowatt-hour and a euro
        // this daemon is entitled to invent.
        let advice = |at_risk: AtRisk, headline: &str| Advice {
            specialist: "s".into(),
            headline: headline.into(),
            at_risk,
            evidence: Vec::new(),
            covers: 1,
            suggested: String::new(),
        };
        let proposal = Proposal {
            advice: vec![
                advice(AtRisk::Energy(kwh("10")), "small energy"),
                advice(
                    AtRisk::Count {
                        n: 99,
                        of: "stations".into(),
                    },
                    "many stations",
                ),
                advice(AtRisk::Energy(kwh("400")), "large energy"),
                advice(
                    AtRisk::Money(Money::new(Decimal::from(5), Currency::EUR)),
                    "small money",
                ),
            ],
            considered: 4,
        }
        .ranked();

        let order: Vec<&str> = proposal
            .advice
            .iter()
            .map(|a| a.headline.as_str())
            .collect();
        assert_eq!(
            order,
            vec![
                "small money",
                "large energy",
                "small energy",
                "many stations"
            ],
            "money first, then energy largest-first, then counts"
        );
    }

    #[test]
    fn a_line_names_the_specialist_the_quantity_and_the_span() {
        let proposal = Proposal {
            advice: vec![Advice {
                specialist: "evidence-triage".into(),
                headline: "one meter accounts for most of today's refusals".into(),
                at_risk: AtRisk::Energy(kwh("412.5")),
                evidence: vec!["s-1".into()],
                covers: 380,
                suggested: "check the meter".into(),
            }],
            considered: 400,
        };
        let line = proposal.lines().next().unwrap();
        assert!(line.contains("evidence-triage"), "{line}");
        assert!(line.contains("412.5 kWh"), "{line}");
        assert!(line.contains("380"), "{line}");
    }

    #[test]
    fn two_currencies_are_two_queues_and_not_one_exchange_rate() {
        // The module refuses to rank a kilowatt-hour against a euro. €100
        // against CHF 100 is the same refusal one level down, and the first
        // version of `ranked` did not make it: sorting `AtRisk::Money` by
        // amount alone invents the rate it says it will not invent (D189).
        let money = |amount: &str, code: &str| Advice {
            specialist: "settlement".to_owned(),
            headline: format!("{amount} {code}"),
            at_risk: AtRisk::Money(Money::new(
                Decimal::from_str_exact(amount).unwrap(),
                Currency::new(code).unwrap(),
            )),
            evidence: Vec::new(),
            covers: 1,
            suggested: "chase it".to_owned(),
        };

        let ranked = Proposal {
            advice: vec![
                money("10", "CHF"),
                money("100", "EUR"),
                money("500", "CHF"),
                money("900", "EUR"),
            ],
            considered: 4,
        }
        .ranked();

        let order: Vec<&str> = ranked.advice.iter().map(|a| a.headline.as_str()).collect();
        assert_eq!(
            order,
            vec!["500 CHF", "10 CHF", "900 EUR", "100 EUR"],
            "one queue per currency, each ordered by its own amount"
        );
    }

    #[test]
    fn counts_of_different_things_are_not_ranked_against_each_other() {
        // 400 sessions and 5 charge points are both counts, and saying the
        // first matters eighty times more than the second is not a fact.
        let count = |n: usize, of: &str| Advice {
            specialist: "triage".to_owned(),
            headline: format!("{n} {of}"),
            at_risk: AtRisk::Count {
                n,
                of: of.to_owned(),
            },
            evidence: Vec::new(),
            covers: n,
            suggested: "look".to_owned(),
        };

        let ranked = Proposal {
            advice: vec![
                count(400, "sessions"),
                count(5, "charge points"),
                count(9, "charge points"),
                count(12, "sessions"),
            ],
            considered: 4,
        }
        .ranked();

        let order: Vec<&str> = ranked.advice.iter().map(|a| a.headline.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "9 charge points",
                "5 charge points",
                "400 sessions",
                "12 sessions"
            ],
            "one queue per counted noun"
        );
    }
}
