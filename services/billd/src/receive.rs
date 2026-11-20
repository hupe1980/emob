//! `POST /v1/deliveries` — a `de.emob.cdr.issued` `CloudEvent` from `csmsd`.
//!
//! The order is the point: the body is capped before it is read (SR-038), the
//! signature and its freshness are checked before the body is parsed (SR-029,
//! SR-030), the record is journalled, and only then is the sender answered —
//! so a `200` is a promise the journal keeps, and `csmsd` records the delivery
//! as made only after it.
//!
//! A refusal before the journal is counted and never journalled: a forger
//! must not be able to grow the books.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use emob_core::PartyId;
use emob_service::head::Latest;
use emob_service::journal::Journal;
use emob_service::page::{self, Page, Refused};
use emob_service::webhook;
use emob_service::{Actor, Operators};
use serde::{Deserialize, Serialize};

use crate::Books;

/// The largest delivery a receiver reads.
pub const BODY_LIMIT: usize = 1024 * 1024;

/// How far a delivery's signature may be from this clock.
pub const TOLERANCE: time::Duration = time::Duration::minutes(5);

/// What the delivery says about who pays.
#[derive(Debug, Deserialize)]
pub struct Token {
    /// The provider that issued the token.
    pub issuer: PartyId,
}

/// The parts of a `de.emob.cdr.issued` event's data `billd` reads
/// (`specs/001-the-estate-runs/contracts/delivery.md`).
#[derive(Debug, Deserialize)]
pub struct Body {
    /// The record.
    pub cdr: emob_cdr::Cdr,
    /// Who pays.
    pub token: Token,
    /// The driver's receipt link, added by `csmsd` at dispatch (spec 003);
    /// it goes onto the invoice line.
    #[serde(default)]
    pub receipt: Option<String>,
}

/// What the routes share.
pub struct Shared<J: Journal> {
    /// The books.
    pub books: Arc<Mutex<Books<J>>>,
    /// The secret deliveries are signed with.
    pub secret: webhook::Secret,
    /// The partners an invoice may be addressed to.
    pub partners: Vec<PartyId>,
    /// Deliveries refused before the journal.
    pub refused: Arc<AtomicU64>,
}

impl<J: Journal> Clone for Shared<J> {
    fn clone(&self) -> Self {
        Self {
            books: Arc::clone(&self.books),
            secret: self.secret.clone(),
            partners: self.partners.clone(),
            refused: Arc::clone(&self.refused),
        }
    }
}

fn lock<T>(held: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    held.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The answer to a delivery.
#[derive(Debug, Serialize)]
pub struct Receipt {
    /// Whether this id had been taken before.
    pub duplicate: bool,
}

fn refuse(status: StatusCode, reason: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "reason": reason.into() }))).into_response()
}

async fn deliveries<J: Journal>(
    State(shared): State<Shared<J>>,
    request: axum::extract::Request,
) -> Response {
    // Capped before it is read (SR-038).
    let (headers, body) = match emob_service::serve::read_bounded(request, BODY_LIMIT).await {
        Ok(read) => read,
        Err(status) => {
            shared.refused.fetch_add(1, Ordering::Relaxed);
            return refuse(status, format!("a delivery is at most {BODY_LIMIT} bytes"));
        }
    };
    let now = time::OffsetDateTime::now_utc();
    let id = match webhook::receive(
        &headers,
        &body,
        std::slice::from_ref(&shared.secret),
        now,
        TOLERANCE,
    ) {
        Ok(id) => id,
        Err(refusal) => {
            shared.refused.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(%refusal, "a delivery was refused before it was read");
            return refuse(StatusCode::UNAUTHORIZED, refusal.to_string());
        }
    };
    // A `de.emob.cdr.issued` CloudEvent and nothing else; the record is its data.
    let body: Body =
        match emob_service::cloudevent::open::<Body>(&body, emob_service::events::cdr::ISSUED) {
            Ok(event) => event.data,
            Err(refusal) => return refuse(StatusCode::UNPROCESSABLE_ENTITY, refusal.to_string()),
        };
    if body.cdr.key.to_string() != id {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "the delivery id {id} is not the record's key {}: the id is the idempotency key, \
                 and one that differs would let a second copy in",
                body.cdr.key
            ),
        );
    }
    if !shared.partners.contains(&body.token.issuer) {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "{} is not a partner in the estate, so there is nobody to invoice this record to",
                body.token.issuer
            ),
        );
    }
    // The signature above is `csmsd`'s, so the receipt is `csmsd`'s fact —
    // never whoever the body says it is about.
    match lock(&shared.books).receive(
        &Actor::service("csmsd"),
        body.cdr,
        body.token.issuer,
        body.receipt,
    ) {
        Ok(fresh) => Json(Receipt { duplicate: !fresh }).into_response(),
        Err(error) => refuse(StatusCode::SERVICE_UNAVAILABLE, error.to_string()),
    }
}

/// What a caller may ask of the view: nothing else is accepted (SR-089).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    /// Rows, `1..=200`, default 50.
    pub limit: Option<usize>,
    /// Documents strictly older than this position.
    pub invoices: Option<u64>,
}

/// The bounded read model (spec 007): counts, and the newest documents still
/// waiting on their recipient.
#[derive(Debug, Serialize)]
pub struct Today {
    /// The rows this answer was cut at.
    pub limit: usize,
    /// Records delivered.
    pub received: u64,
    /// Deliveries refused before the journal since this process started.
    pub refused_deliveries: u64,
    /// Documents their recipient has not accepted, newest first.
    pub invoices: Page<DocumentView>,
}

/// One document waiting on its recipient.
#[derive(Debug, Serialize)]
pub struct DocumentView {
    /// Its number.
    pub number: String,
    /// What it totals, gross.
    pub total: String,
    /// Where it has got to with its recipient.
    pub submission: crate::Submission,
}

impl Today {
    /// The bounded view of one set of books.
    ///
    /// Documents grow with closings — partners times months — not with
    /// charges, so the waiting ones are found by walking the documents.
    ///
    /// # Errors
    ///
    /// [`Refused`] for a `limit` outside `1..=200` (SR-088).
    pub fn of<J: Journal>(
        books: &Books<J>,
        refused: u64,
        params: &Params,
    ) -> Result<Self, Refused> {
        let limit = page::limit(params.limit)?;
        let state = books.state();
        let waiting = |document: &&crate::Issued| !document.submission.is_accepted();
        let total = state.documents().filter(waiting).count() as u64;
        let before = params.invoices.unwrap_or(u64::MAX);
        let invoices = Page::newest(
            state
                .documents()
                .enumerate()
                .map(|(position, document)| (position as u64, document))
                .filter(|(position, document)| *position < before && waiting(document))
                .map(|(position, document)| {
                    (
                        position,
                        DocumentView {
                            number: document.invoice.number.clone(),
                            total: document.invoice.payable_total().to_string(),
                            submission: document.submission.clone(),
                        },
                    )
                }),
            total,
            limit,
        );
        Ok(Self {
            limit,
            received: state.received_count() as u64,
            refused_deliveries: refused,
            invoices,
        })
    }
}

async fn today<J: Journal>(
    State(shared): State<Shared<J>>,
    params: Result<Query<Params>, QueryRejection>,
) -> Response {
    let params = match params {
        Ok(Query(params)) => params,
        Err(rejection) => return Refused::from(rejection).into_response(),
    };
    match Today::of(
        &lock(&shared.books),
        shared.refused.load(Ordering::Relaxed),
        &params,
    ) {
        Ok(view) => Json(view).into_response(),
        Err(refused) => refused.into_response(),
    }
}

/// The receiver, open to signed deliveries, and the view and the signed
/// journal head, behind the operators (SR-103).
pub fn router<J: Journal>(shared: Shared<J>, head: Latest, operators: Operators) -> axum::Router {
    let view = axum::Router::new()
        .route("/v1/billd/today", get(today::<J>))
        .with_state(shared.clone())
        .merge(emob_service::head::router("billd", head));
    axum::Router::new()
        .route(
            "/v1/deliveries",
            post(deliveries::<J>).layer(DefaultBodyLimit::disable()),
        )
        .with_state(shared)
        .merge(emob_service::serve::operator_only(view, operators))
}
