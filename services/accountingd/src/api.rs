//! All balances, bank entries, reviews and confirmations require the finance grant.
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, header::AUTHORIZATION};
use axum::response::Response;
use axum::{Extension, Json, Router, middleware, routing::post};
use emob_core::Currency;
use emob_service::{Actor, estate::valid_account_id, ui};
use serde::Deserialize;
use serde_json::Value;

use crate::ledger::{BAD, Book, OFFLINE, Result};
use crate::model::{Allocation, Import, Offset, OffsetReceipt, Preview};
use crate::{Disk, import::Report};

pub(crate) struct Owner {
    pub accounts: std::collections::BTreeSet<String>,
    pub client: reqwest::Client,
    pub feed: reqwest::Url,
    pub syncing: tokio::sync::Mutex<()>,
    pub ledger: Mutex<Book<Disk>>,
}

impl Owner {
    pub fn book(&self) -> Result<MutexGuard<'_, Book<Disk>>> {
        self.ledger.lock().map_err(|_| OFFLINE)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    account: String,
    currency: Currency,
    #[serde(default)]
    page: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentScope {
    account: String,
    currency: Currency,
    number: String,
    #[serde(default)]
    page: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    source: String,
    account: String,
    currency: Currency,
    reference: String,
    number: String,
    amount_minor: i64,
    review: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OffsetConfirmation {
    #[serde(flatten)]
    selection: Offset,
    review: String,
}

pub(crate) fn routes(owner: Owner, operators: emob_service::Operators) -> Router {
    let owner = Arc::new(owner);
    let finance = Router::new()
        .route("/v1/accountingd/balance", post(balance))
        .route("/v1/accountingd/withdrawal/evidence", post(evidence))
        .route("/v1/accountingd/import", post(import))
        .route("/v1/accountingd/review", post(review))
        .route("/v1/accountingd/confirm", post(confirm))
        .with_state(Arc::clone(&owner));
    let offset_writes = Router::new()
        .route("/v1/accountingd/offsets/review", post(review_offset))
        .route("/v1/accountingd/offsets/confirm", post(confirm_offset))
        .with_state(Arc::clone(&owner));
    let finance = finance.merge(emob_service::serve::operator_write(
        offset_writes,
        operators.clone(),
    ));
    emob_service::serve::finance_only(finance, operators)
        .merge(
            Router::new()
                .route(
                    "/v1/accountingd/customer/balance",
                    post(crate::customer::balance),
                )
                .with_state(owner),
        )
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::map_response(private))
}

async fn private(response: Response) -> Response {
    let mut response = ui::hardened(response, ui::CSP);
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

pub(crate) fn bearer(headers: &HeaderMap) -> Result<&HeaderValue> {
    headers
        .get(AUTHORIZATION)
        .ok_or(axum::http::StatusCode::UNAUTHORIZED)
}

async fn balance(
    State(state): State<Arc<Owner>>,
    headers: HeaderMap,
    Json(scope): Json<Scope>,
) -> Result<Json<Value>> {
    if !valid_account_id(&scope.account) {
        return Err(BAD);
    }
    state.sync(bearer(&headers)?).await?;
    let book = state.book()?;
    book.live()?;
    if !state.accounts.contains(&scope.account) && !book.state.has_account(&scope.account) {
        return Err(axum::http::StatusCode::NOT_FOUND);
    }
    Ok(Json(book.state.balance(
        &scope.account,
        scope.currency,
        scope.page,
    )))
}

async fn evidence(
    State(state): State<Arc<Owner>>,
    headers: HeaderMap,
    Json(scope): Json<DocumentScope>,
) -> Result<Json<Value>> {
    if !valid_account_id(&scope.account) || scope.number.is_empty() || scope.number.len() > 96 {
        return Err(BAD);
    }
    state.sync(bearer(&headers)?).await?;
    let book = state.book()?;
    book.live()?;
    book.state
        .allocation_evidence(&scope.account, scope.currency, &scope.number, scope.page)
        .map(Json)
}

async fn import(
    State(state): State<Arc<Owner>>,
    Extension(actor): Extension<Actor>,
    Json(statement): Json<Import>,
) -> Result<Json<Report>> {
    if !state.accounts.contains(&statement.account) {
        return Err(BAD);
    }
    Ok(Json(state.book()?.import(&actor, statement)?))
}

async fn review(
    State(state): State<Arc<Owner>>,
    Extension(actor): Extension<Actor>,
    headers: HeaderMap,
    Json(selection): Json<Allocation>,
) -> Result<Json<Preview>> {
    state.sync(bearer(&headers)?).await?;
    let book = state.book()?;
    book.live()?;
    Ok(Json(book.review(&actor, &selection)?))
}

async fn confirm(
    State(state): State<Arc<Owner>>,
    Extension(actor): Extension<Actor>,
    headers: HeaderMap,
    Json(input): Json<Confirmation>,
) -> Result<Json<Preview>> {
    let request = Allocation {
        source: input.source,
        reference: input.reference,
        number: input.number,
        account: input.account,
        currency: input.currency,
        amount_minor: input.amount_minor,
    };
    if let Some(posted) = state.book()?.receipt(&actor, &request, &input.review)? {
        return Ok(Json(posted));
    }
    state.sync(bearer(&headers)?).await?;
    Ok(Json(state.book()?.confirm(
        &actor,
        request,
        &input.review,
    )?))
}

async fn review_offset(
    State(owner): State<Arc<Owner>>,
    Extension(actor): Extension<Actor>,
    headers: HeaderMap,
    Json(selection): Json<Offset>,
) -> Result<Json<OffsetReceipt>> {
    owner.sync(bearer(&headers)?).await?;
    owner.book()?.offset_review(&actor, &selection).map(Json)
}

async fn confirm_offset(
    State(owner): State<Arc<Owner>>,
    Extension(actor): Extension<Actor>,
    headers: HeaderMap,
    Json(input): Json<OffsetConfirmation>,
) -> Result<Json<OffsetReceipt>> {
    if let Some(receipt) = owner
        .book()?
        .offset_receipt(&actor, &input.selection, &input.review)?
    {
        return Ok(Json(receipt));
    }
    owner.sync(bearer(&headers)?).await?;
    owner
        .book()?
        .confirm_offset(&actor, &input.selection, &input.review)
        .map(Json)
}
