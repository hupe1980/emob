//! The signed agreement owner records a withdrawal decision and changes online standing atomically.
mod http;
pub(crate) use http::routes;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use axum::http::StatusCode;
use emob_service::Actor;
use emob_service::estate::RetailAccountKind;
use emob_service::journal::Journal;
use emob_service::vault::Sealed;
use emob_session::TokenRef;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::{
    Book, Empd, Event,
    enrol::{Catalog, handle_valid},
};

const PURPOSE: &str = "empd/withdrawal/v1";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Channel {
    Email,
    Letter,
    Phone,
    InPerson,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Hold,
    Approve,
    Decline,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Billing {
    Unaffected,
    ReviewRequired,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stage {
    Received,
    Held,
    Approved,
    Declined,
}

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct Intake {
    agreement: String,
    request_ref: String,
    #[serde(with = "time::serde::rfc3339")]
    received_at: OffsetDateTime,
    channel: Channel,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    agreement: String,
    outcome: Outcome,
    evidence_ref: String,
    billing: Billing,
}

#[derive(Clone, Deserialize, Serialize)]
struct Decision {
    #[serde(flatten)]
    proposal: Proposal,
    actor: Actor,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
    operation: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct Case {
    #[serde(flatten)]
    intake: Intake,
    account: String,
    offer_reference: String,
    #[serde(with = "time::serde::rfc3339")]
    accepted_at: OffsetDateTime,
    recorded_by: Actor,
    #[serde(with = "time::serde::rfc3339")]
    recorded_at: OffsetDateTime,
    stage: Stage,
    decision: Option<Decision>,
}

#[derive(Clone, Serialize)]
pub(crate) struct Review {
    operation: String,
    #[serde(flatten)]
    proposal: Proposal,
    blocks_online: bool,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    #[serde(skip)]
    started: Instant,
    #[serde(skip)]
    last_decision: Option<String>,
    #[serde(skip)]
    active_card: Option<(TokenRef, u64)>,
}

#[derive(Clone, Default)]
pub(crate) struct Cases {
    by_agreement: BTreeMap<String, Case>,
    references: BTreeSet<String>,
    receipts: BTreeMap<String, Case>,
    pub(crate) unreadable: bool,
}

impl std::fmt::Debug for Cases {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cases")
            .field("count", &self.by_agreement.len())
            .finish_non_exhaustive()
    }
}

impl Cases {
    pub(crate) fn stage(&self, agreement: &str) -> Option<Stage> {
        self.by_agreement.get(agreement).map(|case| case.stage)
    }

    pub(crate) fn approved(&self, agreement: &str) -> bool {
        self.stage(agreement) == Some(Stage::Approved)
    }
}

impl Empd {
    pub(crate) fn ended_at(&self, id: &str) -> Option<OffsetDateTime> {
        let withdrawal = self
            .withdrawals
            .by_agreement
            .get(id)
            .filter(|case| case.stage == Stage::Approved)
            .and_then(|case| case.decision.as_ref().map(|decision| decision.at));
        self.changes
            .ended_at(id)
            .into_iter()
            .chain(withdrawal)
            .min()
    }

    pub(crate) fn apply_withdrawal(&mut self, sealed: &Sealed) {
        let case = self
            .key
            .open(PURPOSE, sealed)
            .ok()
            .and_then(|raw| serde_json::from_slice::<Case>(&raw).ok());
        let Some(case) = case else {
            self.withdrawals.unreadable = true;
            return;
        };
        let agreement = &case.intake.agreement;
        if case.stage == Stage::Approved {
            // Ordinary termination might have retired the last card before this decision.
            let Some(sale) = self.agreements.receipt(agreement) else {
                self.withdrawals.unreadable = true;
                return;
            };
            if let Ok(active) = self.changes.active(sale) {
                let active = active.clone();
                if self.block(&active).is_err() {
                    self.withdrawals.unreadable = true;
                    return;
                }
                self.changes.retire(active);
            }
        }
        if let Some(decision) = &case.decision {
            self.withdrawals
                .receipts
                .insert(decision.operation.clone(), case.clone());
        }
        self.withdrawals
            .references
            .insert(case.intake.request_ref.clone());
        self.withdrawals
            .by_agreement
            .insert(agreement.clone(), case);
    }

    fn active_card_revision(&self, id: &str) -> Option<(TokenRef, u64)> {
        let card = self.changes.active(self.agreements.receipt(id)?).ok()?;
        Some((card.clone(), self.token(card)?.revision))
    }
}

fn reference(value: &str) -> bool {
    value.len() <= 96
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/:-".contains(&b))
}

impl<J: Journal> Book<J> {
    fn case(&self, id: &str) -> Result<&Case, StatusCode> {
        self.guard().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        if !handle_valid(id) {
            return Err(StatusCode::BAD_REQUEST);
        }
        self.state()
            .withdrawals
            .by_agreement
            .get(id)
            .ok_or(StatusCode::NOT_FOUND)
    }

    fn save_case(&mut self, actor: &Actor, case: &Case) -> Result<(), StatusCode> {
        let bytes = serde_json::to_vec(case).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let evidence = self.state().key.seal(PURPOSE, &bytes);
        self.record_fact(actor, Event::Withdrawal { evidence })
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
    }

    fn intake(
        &mut self,
        actor: &Actor,
        request: Intake,
        catalog: &Catalog,
        now: OffsetDateTime,
    ) -> Result<Case, StatusCode> {
        let agreement = self
            .receipt(&request.agreement)?
            .ok_or(StatusCode::NOT_FOUND)?;
        if !reference(&request.request_ref) {
            return Err(StatusCode::BAD_REQUEST);
        }
        if let Some(existing) = self
            .state()
            .withdrawals
            .by_agreement
            .get(&request.agreement)
        {
            return if existing.intake == request {
                Ok(existing.clone())
            } else {
                Err(StatusCode::CONFLICT)
            };
        }
        if self
            .state()
            .withdrawals
            .references
            .contains(&request.request_ref)
        {
            return Err(StatusCode::CONFLICT);
        }
        if request.received_at < agreement.at || request.received_at > now {
            return Err(StatusCode::BAD_REQUEST);
        }
        // Earlier sales pinned the bill-to profile but did not record its classification.
        let kind = if let Some(kind) = agreement.account_kind {
            kind
        } else {
            let payer = catalog.account(&agreement.account)?;
            self.profile_digest(payer)?;
            payer.kind
        };
        if kind != RetailAccountKind::Private {
            return Err(StatusCode::UNPROCESSABLE_ENTITY);
        }
        let case = Case {
            intake: request,
            account: agreement.account,
            offer_reference: agreement.offer.reference,
            accepted_at: agreement.at,
            recorded_at: now,
            recorded_by: actor.clone(),
            stage: Stage::Received,
            decision: None,
        };
        self.save_case(actor, &case)?;
        Ok(case)
    }

    fn withdrawal_review(
        &mut self,
        actor: &Actor,
        proposal: Proposal,
        now: OffsetDateTime,
    ) -> Result<Review, StatusCode> {
        let case = self.case(&proposal.agreement)?;
        if !reference(&proposal.evidence_ref) {
            return Err(StatusCode::BAD_REQUEST);
        }
        if !matches!(case.stage, Stage::Received | Stage::Held)
            || now
                < case
                    .decision
                    .as_ref()
                    .map_or(case.recorded_at, |decision| decision.at)
            || self
                .state()
                .changes
                .latest(&proposal.agreement)
                .is_some_and(|change| change.at > now)
        {
            return Err(StatusCode::CONFLICT);
        }
        let mut nonce = [0_u8; 32];
        aws_lc_rs::rand::fill(&mut nonce).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let review = Review {
            operation: hex::encode(nonce),
            blocks_online: proposal.outcome == Outcome::Approve,
            expires_at: now + Duration::minutes(5),
            started: Instant::now(),
            last_decision: case
                .decision
                .as_ref()
                .map(|decision| decision.operation.clone()),
            active_card: self.state().active_card_revision(&proposal.agreement),
            proposal,
        };
        self.withdrawal_reviews
            .insert(actor.clone(), review.clone());
        Ok(review)
    }

    fn confirm_withdrawal(
        &mut self,
        actor: &Actor,
        agreement: &str,
        operation: &str,
        now: OffsetDateTime,
    ) -> Result<Case, StatusCode> {
        let mut case = self.case(agreement)?.clone();
        if !handle_valid(operation) {
            return Err(StatusCode::BAD_REQUEST);
        }
        if let Some(done) = self.state().withdrawals.receipts.get(operation) {
            return if done.intake.agreement == agreement
                && done.decision.as_ref().is_some_and(|d| &d.actor == actor)
            {
                Ok(done.clone())
            } else {
                Err(StatusCode::CONFLICT)
            };
        }
        let review = self
            .withdrawal_reviews
            .get(actor)
            .filter(|r| {
                r.operation == operation
                    && r.proposal.agreement == agreement
                    && now >= r.expires_at - Duration::minutes(5)
                    && now < r.expires_at
                    && r.started.elapsed() < std::time::Duration::from_mins(5)
                    && r.last_decision.as_ref() == case.decision.as_ref().map(|d| &d.operation)
                    && r.active_card == self.state().active_card_revision(agreement)
            })
            .ok_or(StatusCode::CONFLICT)?;
        case.stage = match review.proposal.outcome {
            Outcome::Hold => Stage::Held,
            Outcome::Approve => Stage::Approved,
            Outcome::Decline => Stage::Declined,
        };
        case.decision = Some(Decision {
            proposal: review.proposal.clone(),
            actor: actor.clone(),
            at: now,
            operation: operation.into(),
        });
        self.save_case(actor, &case)?;
        self.withdrawal_reviews.remove(actor);
        Ok(case)
    }
}
