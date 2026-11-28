//! Replace the active online card or end the agreement, retaining the original sale.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use axum::http::StatusCode;
use emob_service::{Actor, journal::Journal};
use emob_session::TokenRef;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::{
    Agreement, Book, Empd, Event, Token, TokenType, Whitelist, cards::label, enrol::handle_valid,
};

/// A reviewed change to an existing native agreement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    /// Retire the active card and issue another on the same contract.
    Replace,
    /// Stop online use and future recurring charges.
    End,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub(crate) agreement: String,
    pub(crate) action: ChangeKind,
    pub(crate) stock: Option<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct Review {
    pub(crate) operation: String,
    agreement: String,
    action: ChangeKind,
    account: String,
    current_card: String,
    replacement_card: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    #[serde(skip)]
    started: Instant,
    #[serde(skip)]
    previous: TokenRef,
    #[serde(skip)]
    revision: u64,
    #[serde(skip)]
    replacement: Option<TokenRef>,
    #[serde(skip)]
    pub(crate) stock: Option<String>,
}

/// The repeatable confirmation receipt, containing masked card labels.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgreementChange {
    /// Unique id for retrying the same operator's confirmation.
    pub operation: String,
    /// Issuance operation identifying the original contract.
    pub agreement: String,
    /// Effective action.
    pub action: ChangeKind,
    /// Immutable bill-to account.
    pub account: String,
    /// Retired card's masked label.
    pub previous_card: String,
    /// New active card's masked label after replacement.
    pub current_card: Option<String>,
    /// Grant used to commit the change.
    pub actor: Actor,
    /// Effective instant for online use and termination.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// Each confirmed operation can be retried after a process restart.
#[derive(Debug, Clone, Default)]
pub(crate) struct Changes {
    latest: BTreeMap<String, (String, Option<TokenRef>)>,
    confirmed: BTreeMap<String, AgreementChange>,
    retired: BTreeSet<TokenRef>,
}

impl Changes {
    pub(crate) fn latest(&self, id: &str) -> Option<&AgreementChange> {
        self.latest
            .get(id)
            .and_then(|(operation, _)| self.confirmed.get(operation))
    }

    pub(crate) fn retired(&self, card: &TokenRef) -> bool {
        self.retired.contains(card)
    }

    pub(crate) fn ended_at(&self, id: &str) -> Option<OffsetDateTime> {
        self.latest(id)
            .filter(|c| c.action == ChangeKind::End)
            .map(|c| c.at)
    }

    pub(crate) fn retire(&mut self, card: TokenRef) {
        self.retired.insert(card);
    }

    pub(crate) fn active<'a>(
        &'a self,
        agreement: &'a Agreement,
    ) -> Result<&'a TokenRef, StatusCode> {
        match self.latest.get(&agreement.agreement) {
            Some((_, current)) => current.as_ref().ok_or(StatusCode::CONFLICT),
            None => Ok(&agreement.reference),
        }
    }
}

impl Empd {
    pub(crate) fn apply_contract_change(
        &mut self,
        change: AgreementChange,
        previous: TokenRef,
        replacement: Option<(TokenRef, Token)>,
    ) {
        if let Some(mut old) = self.token(&previous).cloned() {
            old.blocked = true;
            self.store_card(&previous, old);
        }
        self.changes.retired.insert(previous);
        let next = replacement.map(|(reference, held)| {
            self.agreements
                .attach_replacement(&change.agreement, &reference, &held);
            self.store_card(&reference, held);
            reference
        });
        self.changes
            .latest
            .insert(change.agreement.clone(), (change.operation.clone(), next));
        self.changes
            .confirmed
            .insert(change.operation.clone(), change);
    }
}

impl<J: Journal> Book<J> {
    pub(crate) fn preview_lifecycle(
        &mut self,
        actor: &Actor,
        input: Request,
        uid: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<Review, StatusCode> {
        let agreement = self
            .receipt(&input.agreement)?
            .ok_or(StatusCode::NOT_FOUND)?;
        if agreement.account.is_empty() || self.state().withdrawals.approved(&input.agreement) {
            return Err(StatusCode::CONFLICT);
        }
        let previous = self.state().changes.active(&agreement)?;
        if now
            < self
                .state()
                .changes
                .latest(&input.agreement)
                .map_or(agreement.at, |c| c.at)
        {
            return Err(StatusCode::CONFLICT);
        }
        let replacement = match (input.action, input.stock.as_deref(), uid) {
            (ChangeKind::Replace, Some(code), Some(uid)) => {
                Some(self.state().available_stock(code, uid)?)
            }
            (ChangeKind::End, None, None) => None,
            _ => return Err(StatusCode::BAD_REQUEST),
        };
        let mut nonce = [0_u8; 32];
        aws_lc_rs::rand::fill(&mut nonce).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let review = Review {
            operation: hex::encode(nonce),
            agreement: input.agreement,
            action: input.action,
            account: agreement.account.clone(),
            current_card: label(previous),
            replacement_card: replacement.as_ref().map(label),
            expires_at: now + Duration::minutes(5),
            started: Instant::now(),
            previous: previous.clone(),
            revision: self
                .state()
                .token(previous)
                .ok_or(StatusCode::CONFLICT)?
                .revision,
            replacement,
            stock: input.stock,
        };
        self.lifecycle_reviews.insert(actor.clone(), review.clone());
        Ok(review)
    }

    pub(crate) fn confirm_lifecycle(
        &mut self,
        actor: &Actor,
        agreement_id: &str,
        operation: &str,
        catalog: &crate::enrol::Catalog,
        now: OffsetDateTime,
    ) -> Result<AgreementChange, StatusCode> {
        self.guard().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        if !handle_valid(agreement_id) || !handle_valid(operation) {
            return Err(StatusCode::BAD_REQUEST);
        }
        if let Some(found) = self.state().changes.confirmed.get(operation) {
            return if &found.actor == actor && found.agreement == agreement_id {
                Ok(found.clone())
            } else {
                Err(StatusCode::CONFLICT)
            };
        }
        let review = self
            .lifecycle_reviews
            .get(actor)
            .filter(|r| {
                r.operation == operation
                    && r.agreement == agreement_id
                    && now >= r.expires_at - Duration::minutes(5)
                    && now < r.expires_at
                    && r.started.elapsed() < std::time::Duration::from_mins(5)
            })
            .ok_or(StatusCode::CONFLICT)?;
        let agreement = self
            .receipt(&review.agreement)?
            .ok_or(StatusCode::NOT_FOUND)?;
        if self.state().withdrawals.approved(agreement_id) {
            return Err(StatusCode::CONFLICT);
        }
        let prior = self.state().changes.active(&agreement)?;
        if prior != &review.previous
            || self
                .state()
                .token(prior)
                .is_none_or(|c| c.revision != review.revision)
        {
            return Err(StatusCode::CONFLICT);
        }
        let next = if let Some(stock) = &review.stock {
            let uid = catalog.stock_uid(stock)?;
            let reference = self.state().available_stock(stock, uid)?;
            if Some(&reference) != review.replacement.as_ref() {
                return Err(StatusCode::CONFLICT);
            }
            let mut held = self
                .state()
                .prepare_card(&agreement.emaid, uid, TokenType::Rfid, Whitelist::Never)
                .map_err(|_| StatusCode::BAD_REQUEST)?;
            held.visual_digest = self.state().printed_digest(stock);
            Some((reference, held))
        } else {
            None
        };
        let change = AgreementChange {
            operation: operation.to_owned(),
            agreement: review.agreement.clone(),
            action: review.action,
            account: agreement.account.clone(),
            previous_card: review.current_card.clone(),
            current_card: review.replacement_card.clone(),
            actor: actor.clone(),
            at: now,
        };
        self.record_fact(
            actor,
            Event::AgreementChanged {
                change: Box::new(change.clone()),
                previous: prior.clone(),
                replacement: next.map(Box::new),
            },
        )
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        self.lifecycle_reviews.remove(actor);
        Ok(change)
    }
}
