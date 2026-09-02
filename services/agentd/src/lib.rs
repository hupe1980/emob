//! `agentd` — the advisory plane for emob.
//!
//! # What it is for
//!
//! Every crate below this one answers a question about **one** thing: is this
//! chain sound, does this record add up, is this tariff lawful at this power.
//! Those answers are exact and they are the ones that decide money.
//!
//! None of them answers the question an operator actually has, which is a
//! question about a **population**: of four hundred refused sessions this
//! morning, which one fault caused most of them; of an estate's tariffs and the
//! points that offer them, which pairings breach. Those answers are correlations
//! across many exact answers, and nothing else in the workspace is positioned to
//! make one.
//!
//! # Advisory only, and it is a property
//!
//! An agent **proposes**; the invariants decide. Two things make that structural
//! rather than a promise, and both are in [`advice`]:
//!
//! * the output type is a leaf — nothing in this workspace consumes an
//!   [`advice::Advice`], so there is no path from an agent's answer into a
//!   document;
//! * a specialist's principal is derived by
//!   [`emob_service::Principal::attenuate`], which refuses to widen, and
//!   [`advice::advisory`] is the only constructor — so no agent principal can
//!   hold a capability that writes, and a test asserts it.
//!
//! # The journal is why this is a runtime and not a cron job
//!
//! The specialists are pure functions. `agentplane` runs them anyway, because
//! what it provides is not inference: the run, its input, its answer and every
//! effect go into an append-only hash-chained log, and a replay re-executes the
//! logic while reading each effect back rather than performing it again. "Why
//! did the queue say that in March" becomes a replay instead of an argument —
//! and for a pure function the replay is exact.
//!
//! # Layout
//!
//! | Module | Purpose |
//! |---|---|
//! | [`advice`] | what a specialist may say, and the reason it cannot say anything else |
//! | [`skills`] | the specialists, whose work is computation |
//! | [`subscriptions`] | which `CloudEvent` type reaches which specialist |

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]

use std::sync::Arc;

use agentplane::prelude::*;

pub mod advice;
pub mod skills;
pub mod subscriptions;

pub use advice::{Advice, AtRisk, Proposal, advisory};
pub use subscriptions::{Subscription, specialists_for};

/// Every specialist this daemon registers, by name.
///
/// One list, read by the runtime builder and by the subscription table's own
/// test — so a specialist that is subscribed and not wired is a build failure
/// rather than a row that dispatches into nothing.
#[must_use]
pub fn registered_specialists() -> Vec<&'static str> {
    vec![skills::evidence::NAME, skills::tariff::NAME]
}

/// Build the runtime with every specialist wired.
///
/// The store is the caller's: an embedded file for a single instance, or a
/// database several instances share. What the daemon owns is which specialists
/// exist, and that is [`registered_specialists`].
#[must_use]
pub fn runtime(store: Arc<dyn JournalStore>) -> Arc<Runtime> {
    Runtime::builder(store)
        .skill(skills::evidence::EvidenceTriage)
        .skill(skills::tariff::TariffReview)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn a_specialist_runs_and_the_run_replays_to_the_same_answer() {
        // The property that makes the journal worth having for a pure function:
        // the replay re-executes the logic and reads every effect back, so
        // "why did the queue say that" is answered rather than argued.
        let store: Arc<dyn JournalStore> =
            Arc::new(RedbStore::open_in_memory().expect("an in-memory journal"));
        let runtime = runtime(Arc::clone(&store));

        let input = json!({
            "sessions": [{
                "session": "s-1",
                "evse_id": "DE*ABC*E00001",
                "signing_component": "BQ1",
                "energy_at_risk": "12.500",
                "reasons": ["the meter was in state Substitute, which may not be billed"]
            }]
        });

        let outcome = runtime
            .run(skills::evidence::NAME, Tainted::trusted(input))
            .await
            .expect("the run completed");
        let run_id = outcome.run_id;
        let output = outcome.output.clone();

        let answer = outcome.success().expect("an answer");
        let proposal: Proposal = serde_json::from_value(answer.peek().clone()).expect("a proposal");
        assert_eq!(proposal.considered, 1);
        assert_eq!(proposal.advice.len(), 1);

        let replayed = runtime
            .replay(run_id, Mode::Strict)
            .await
            .expect("the replay completed");
        assert_eq!(output, replayed.output, "replay reproduced the run");
    }

    #[tokio::test]
    async fn an_unreadable_input_fails_the_run_rather_than_inventing_advice() {
        // A specialist that answered anyway would put a finding in an operator's
        // queue that no domain crate produced.
        let store: Arc<dyn JournalStore> =
            Arc::new(RedbStore::open_in_memory().expect("an in-memory journal"));
        let runtime = runtime(store);

        let outcome = runtime
            .run(
                skills::evidence::NAME,
                Tainted::trusted(json!({ "not": "a refusal list" })),
            )
            .await
            .expect("the run completed");
        assert!(outcome.success().is_err(), "the run did not fail");
    }

    #[test]
    fn every_registered_specialist_is_one_the_runtime_can_run() {
        // The names are a `const` in two places — the registry and the
        // descriptors — and they have to be the same names.
        let registered = registered_specialists();
        assert!(registered.contains(&skills::evidence::NAME));
        assert!(registered.contains(&skills::tariff::NAME));
        assert_eq!(registered.len(), 2, "add a specialist, add it here");
    }
}
