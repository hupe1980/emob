//! A privacy operator reviews the portal bindings and confirms their destruction in independent custody.
use axum::{
    Extension, Json, Router,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    routing::post,
};
use emob_service::{Actor, Operators, journal::Journal};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use time::OffsetDateTime;

use super::{Identity, Kind, Subject, subject_tag};
use crate::{
    Book,
    http::{Shared, lock, payload},
};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Policy {
    PortalAccessV1,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Receipt {
    pub(crate) policy: Policy,
    pub(crate) operation: String,
    pub(crate) actor: Actor,
    #[serde(with = "time::serde::rfc3339")]
    pub(crate) erased_at: OffsetDateTime,
    pub(crate) revision: u64,
    pub(crate) accounts: usize,
    pub(crate) agreements: usize,
}

#[derive(Clone)]
pub(crate) struct Pending {
    tag: String,
    operation: String,
    revision: u64,
    created: Instant,
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Outcome {
    Active {
        #[serde(flatten)]
        counts: Counts,
    },
    Ready {
        #[serde(flatten)]
        counts: Counts,
        operation: String,
    },
    Erased {
        #[serde(flatten)]
        receipt: Receipt,
    },
}

#[derive(Serialize)]
struct Report {
    subject: String,
    #[serde(flatten)]
    outcome: Outcome,
}

#[derive(Serialize)]
struct Counts {
    policy: Policy,
    revision: u64,
    accounts: usize,
    agreements: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    subject: String,
    operation: String,
}

impl<J: Journal> Book<J> {
    fn receipt_for_tag(&self, tag: &str) -> Result<Option<Receipt>, StatusCode> {
        self.state()
            .custody
            .receipt(tag)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
    }

    fn erasure_counts(&self, subject: &str, configured: bool) -> Result<Counts, StatusCode> {
        let current = self.state().grants.subjects.get(subject);
        if current.is_none() && !configured {
            return Err(StatusCode::NOT_FOUND);
        }
        let agreements = current.map_or(0, |a| {
            a.targets
                .keys()
                .filter(|(kind, _)| *kind == Kind::Agreement)
                .count()
        });
        Ok(Counts {
            policy: Policy::PortalAccessV1,
            accounts: current.map_or(0, |a| a.targets.len()) - agreements,
            agreements,
            revision: current.map_or(0, |a| a.revision),
        })
    }

    fn erasure_status(
        &mut self,
        subject: String,
        configured: bool,
        reviewer: Option<&Actor>,
    ) -> Result<Report, StatusCode> {
        self.customer_ready()?;
        let tag = subject_tag(&self.state().key, &subject);
        let outcome = if let Some(receipt) = self.receipt_for_tag(&tag)? {
            Outcome::Erased { receipt }
        } else {
            let counts = self.erasure_counts(&subject, configured)?;
            if let Some(actor) = reviewer {
                let mut bytes = [0u8; 32];
                aws_lc_rs::rand::fill(&mut bytes).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
                let operation = hex::encode(bytes);
                self.erasure_reviews.insert(
                    actor.clone(),
                    Pending {
                        tag,
                        operation: operation.clone(),
                        revision: counts.revision,
                        created: Instant::now(),
                    },
                );
                Outcome::Ready { counts, operation }
            } else {
                Outcome::Active { counts }
            }
        };
        Ok(Report { subject, outcome })
    }

    fn erase_customer(
        &mut self,
        actor: &Actor,
        input: Confirmation,
        customers: &mut Identity,
        now: OffsetDateTime,
    ) -> Result<Report, StatusCode> {
        self.customer_ready()?;
        if !crate::enrol::handle_valid(&input.operation) {
            return Err(StatusCode::BAD_REQUEST);
        }
        let tag = subject_tag(&self.state().key, &input.subject);
        let receipt = if let Some(saved) = self.receipt_for_tag(&tag)? {
            if saved.actor != *actor || saved.operation != input.operation {
                return Err(StatusCode::CONFLICT);
            }
            saved
        } else {
            let counts = self.erasure_counts(&input.subject, customers.contains(&input.subject))?;
            let valid = self.erasure_reviews.get(actor).is_some_and(|pending| {
                pending.tag == tag
                    && pending.operation == input.operation
                    && pending.revision == counts.revision
                    && pending.created.elapsed() < Duration::from_mins(5)
            });
            if !valid {
                return Err(StatusCode::CONFLICT);
            }
            let receipt = Receipt {
                policy: counts.policy,
                actor: actor.clone(),
                operation: input.operation,
                revision: counts.revision,
                accounts: counts.accounts,
                agreements: counts.agreements,
                erased_at: now,
            };
            self.state()
                .custody
                .erase(&tag, &receipt)
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        };
        customers.forget(&input.subject);
        self.erasure_reviews.retain(|_, r| r.tag != tag);
        self.forget_customer_grants(&input.subject);
        if !self.state().grants.witnessed.contains(&tag) {
            self.record_fact(
                actor,
                crate::Event::CustomerErased {
                    subject_tag: tag.clone(),
                    operation: receipt.operation.clone(),
                },
            )
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
            self.customer_ready()?;
        }
        Ok(Report {
            subject: input.subject,
            outcome: Outcome::Erased { receipt },
        })
    }
}

async fn status<J: Journal>(
    State(shared): State<Shared<J>>,
    input: Result<Json<Subject>, JsonRejection>,
) -> Result<Json<Report>, StatusCode> {
    let subject = payload(input)?.subject;
    let configured = lock(&shared.customers).contains(&subject);
    lock(&shared.book)
        .erasure_status(subject, configured, None)
        .map(Json)
}

async fn review<J: Journal>(
    State(shared): State<Shared<J>>,
    Extension(actor): Extension<Actor>,
    input: Result<Json<Subject>, JsonRejection>,
) -> Result<Json<Report>, StatusCode> {
    let subject = payload(input)?.subject;
    let configured = lock(&shared.customers).contains(&subject);
    lock(&shared.book)
        .erasure_status(subject, configured, Some(&actor))
        .map(Json)
}

async fn confirm<J: Journal>(
    State(shared): State<Shared<J>>,
    Extension(actor): Extension<Actor>,
    input: Result<Json<Confirmation>, JsonRejection>,
) -> Result<Json<Report>, StatusCode> {
    let input = payload(input)?;
    let mut book = lock(&shared.book);
    let mut customers = lock(&shared.customers);
    book.erase_customer(&actor, input, &mut customers, (shared.clock)())
        .map(Json)
}

pub(super) fn routes<J: Journal>(shared: Shared<J>, operators: Operators) -> Router {
    let router = Router::new()
        .route("/v1/empd/customer/erasure/status", post(status::<J>))
        .route("/v1/empd/customer/erasure/review", post(review::<J>))
        .route("/v1/empd/customer/erasure/confirm", post(confirm::<J>))
        .with_state(shared);
    emob_service::serve::privacy_only(router, operators)
}
