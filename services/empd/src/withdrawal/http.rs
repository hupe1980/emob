//! Writer-only case API. The case reference travels in the bounded request body.
use axum::{
    Extension, Json, Router,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    routing::post,
};
use emob_service::{Actor, journal::Journal};
use serde::Deserialize;

use super::{Case, Intake, Proposal, Review};
use crate::http::{Shared, lock, payload};

pub(crate) fn routes<J: Journal>() -> Router<Shared<J>> {
    Router::new()
        .route("/v1/empd/contracts/withdrawal/intake", post(intake::<J>))
        .route("/v1/empd/contracts/withdrawal/lookup", post(lookup::<J>))
        .route("/v1/empd/contracts/withdrawal/review", post(review::<J>))
        .route("/v1/empd/contracts/withdrawal/confirm", post(confirm::<J>))
}

async fn intake<J: Journal>(
    State(shared): State<Shared<J>>,
    Extension(actor): Extension<Actor>,
    body: Result<Json<Intake>, JsonRejection>,
) -> Result<Json<Case>, StatusCode> {
    let input = payload(body)?;
    lock(&shared.book)
        .intake(&actor, input, &shared.catalog, (shared.clock)())
        .map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    agreement: String,
}

async fn lookup<J: Journal>(
    State(shared): State<Shared<J>>,
    body: Result<Json<Lookup>, JsonRejection>,
) -> Result<Json<Case>, StatusCode> {
    let input = payload(body)?;
    lock(&shared.book).case(&input.agreement).cloned().map(Json)
}

async fn review<J: Journal>(
    State(shared): State<Shared<J>>,
    Extension(actor): Extension<Actor>,
    body: Result<Json<Proposal>, JsonRejection>,
) -> Result<Json<Review>, StatusCode> {
    let input = payload(body)?;
    lock(&shared.book)
        .withdrawal_review(&actor, input, (shared.clock)())
        .map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    agreement: String,
    operation: String,
}

async fn confirm<J: Journal>(
    State(shared): State<Shared<J>>,
    Extension(actor): Extension<Actor>,
    body: Result<Json<Confirmation>, JsonRejection>,
) -> Result<Json<Case>, StatusCode> {
    let input = payload(body)?;
    lock(&shared.book)
        .confirm_withdrawal(&actor, &input.agreement, &input.operation, (shared.clock)())
        .map(Json)
}
