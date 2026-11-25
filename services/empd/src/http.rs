//! `empd` over HTTP: the one question `csmsd` asks, and the operator's view.
//!
//! - `POST /v1/authorize` — behind the estate's **service** token (SR-033). An
//!   unauthenticated caller would turn the token store into an oracle for which
//!   cards are valid and whose contract they bill.
//! - `GET /v1/empd` — behind the **operator** tokens (SR-034), and without a
//!   single uid: the reference a session holds is what an operator needs.
//! - `POST /v1/empd/cards/lookup` — a bounded operator search with sensitive
//!   lookup terms in the request body and private, non-cacheable responses.
//! - Card preview and confirmation require the named operator’s write permission.

use std::sync::{Arc, Mutex};

use axum::extract::{DefaultBodyLimit, State, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Extension, Json};
use emob_core::{ContractId, EvseId, PartyId};
use emob_roam::TokenType;
use emob_service::journal::Journal;
use emob_service::{Operators, Token};
use serde::{Deserialize, Serialize};

use crate::{Allowed, Book, BookError, ProviderError, Whitelist};

/// A request to authorise one uid.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// The uid the station read.
    pub uid: String,
    /// Online charging point for a point-bound native agreement.
    pub evse: Option<EvseId>,
}

/// The four answers a station understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// Charge.
    Accepted,
    /// Blocked.
    Blocked,
    /// The contract is not in force.
    Expired,
    /// Anything else.
    Invalid,
}

/// The token as this provider presents it to a crossing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presented {
    /// This provider.
    pub issuer: PartyId,
    /// The uid.
    pub uid: String,
    /// What was presented.
    pub token_type: TokenType,
    /// The contract it bills against.
    pub contract_id: ContractId,
}

/// The answer (`specs/archive/001-the-estate-runs/contracts/authorize.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// The status.
    pub status: Status,
    /// Why.
    pub reason: String,
    /// On `Accepted`, whom the session bills.
    pub token: Option<Presented>,
    /// On `Accepted`, the keyed digest a session holds.
    pub reference: Option<String>,
    /// For a native card, the current CSMS offer must match this binding before charging.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub retail: Option<crate::RetailBinding>,
}

fn answer(status: Status, reason: impl Into<String>) -> Answer {
    Answer {
        status,
        reason: reason.into(),
        token: None,
        reference: None,
        retail: None,
    }
}

/// Answer one uid on one day — the rule, without the socket.
pub fn decide<J: Journal>(book: &Book<J>, uid: &str, on: time::Date) -> Answer {
    decide_at(book, uid, None, on.midnight().assume_utc())
}

fn decide_at<J: Journal>(
    book: &Book<J>,
    uid: &str,
    evse: Option<&EvseId>,
    at: time::OffsetDateTime,
) -> Answer {
    let reference = book.state().presented_reference(uid);
    match book
        .guard()
        .and_then(|()| Ok(book.state().authorize_at(&reference, at, evse)?))
    {
        Ok(Allowed::Allowed) => match book.state().present(&reference) {
            Ok(token) => Answer {
                status: Status::Accepted,
                reason: "the contract is in force and the token is not blocked".to_owned(),
                token: Some(Presented {
                    issuer: token.issuer,
                    uid: token.uid,
                    token_type: token.token_type,
                    contract_id: token.contract_id,
                }),
                reference: Some(reference.as_str().to_owned()),
                retail: book
                    .state()
                    .agreements
                    .for_card(&reference)
                    .map(crate::Agreement::binding),
            },
            Err(error) => answer(Status::Invalid, error.to_string()),
        },
        Ok(Allowed::Blocked) => answer(Status::Blocked, "the token is blocked"),
        Ok(Allowed::Expired) => answer(Status::Expired, "the contract is not in force on this day"),
        Ok(other) => answer(Status::Invalid, other.as_str()),
        Err(BookError::Provider(error)) if matches!(*error, ProviderError::UnknownToken { .. }) => {
            answer(
                Status::Invalid,
                "this provider issued no token with that uid",
            )
        }
        Err(BookError::Halted { .. }) => answer(Status::Invalid, "the provider is unavailable"),
        Err(error) => answer(Status::Invalid, error.to_string()),
    }
}

/// What the routes share.
pub struct Shared<J: Journal> {
    /// The book.
    pub book: Arc<Mutex<Book<J>>>,
    /// The token `csmsd` presents.
    pub service: Token,
    pub(crate) catalog: Arc<crate::enrol::Catalog>,
    pub(crate) clock: emob_service::Clock,
}

impl<J: Journal> Clone for Shared<J> {
    fn clone(&self) -> Self {
        Self {
            book: Arc::clone(&self.book),
            service: self.service.clone(),
            catalog: Arc::clone(&self.catalog),
            clock: Arc::clone(&self.clock),
        }
    }
}

fn lock<T>(held: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    held.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn authorize<J: Journal>(
    State(shared): State<Shared<J>>,
    headers: HeaderMap,
    input: Result<Json<Request>, JsonRejection>,
) -> Result<Json<Answer>, StatusCode> {
    let offered = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if !offered.is_some_and(|offered| shared.service.verify(offered)) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let request = payload(input)?;
    Ok(Json(decide_at(
        &lock(&shared.book),
        &request.uid,
        request.evse.as_ref(),
        (shared.clock)(),
    )))
}

/// The read model.
#[derive(Debug, Serialize)]
pub struct View {
    /// The contracts in the book.
    pub contracts: Vec<ContractView>,
    /// The tokens, by reference — never by uid.
    pub tokens: Vec<TokenView>,
}

/// One contract.
#[derive(Debug, Serialize)]
pub struct ContractView {
    /// Its id.
    pub id: String,
    /// From when.
    pub from: String,
}

/// One token, without its uid.
#[derive(Debug, Serialize)]
pub struct TokenView {
    /// The keyed digest.
    pub reference: String,
    /// The contract.
    pub contract: String,
    /// What it is.
    pub token_type: TokenType,
    /// The whitelist.
    pub whitelist: Whitelist,
    /// Whether it is blocked.
    pub blocked: bool,
}

impl View {
    /// The view of one book.
    pub fn of<J: Journal>(book: &Book<J>) -> Self {
        Self {
            contracts: book
                .state()
                .contracts()
                .map(|contract| ContractView {
                    id: contract.id.to_string(),
                    from: contract.from.to_string(),
                })
                .collect(),
            tokens: book
                .state()
                .tokens()
                .map(|(reference, token)| TokenView {
                    reference: reference.to_owned(),
                    contract: token.contract.to_string(),
                    token_type: token.token_type,
                    whitelist: token.whitelist,
                    blocked: token.blocked,
                })
                .collect(),
        }
    }
}

// Map decode errors to a bare status. Some JSON diagnostics quote the value
// that failed, which on a card search may be a private identifier.
async fn lookup<J: Journal>(
    State(shared): State<Shared<J>>,
    query: Result<Json<crate::cards::Query>, JsonRejection>,
) -> Result<Json<crate::cards::Matches>, StatusCode> {
    let query = payload(query)?;
    let book = lock(&shared.book);
    if book.is_halted() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    book.state()
        .lookup_cards(query)
        .map(Json)
        .ok_or(StatusCode::BAD_REQUEST)
}

fn payload<T>(input: Result<Json<T>, JsonRejection>) -> Result<T, StatusCode> {
    input
        .map(|Json(value)| value)
        .map_err(|error| match error.status() {
            StatusCode::PAYLOAD_TOO_LARGE => StatusCode::PAYLOAD_TOO_LARGE,
            StatusCode::UNSUPPORTED_MEDIA_TYPE => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            _ => StatusCode::BAD_REQUEST,
        })
}

fn refused(error: crate::actions::Refusal) -> StatusCode {
    use crate::actions::Refusal;
    match error {
        Refusal::Invalid => StatusCode::BAD_REQUEST,
        Refusal::Unknown => StatusCode::NOT_FOUND,
        Refusal::Stale => StatusCode::CONFLICT,
        Refusal::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn preview<J: Journal>(
    State(shared): State<Shared<J>>,
    Extension(actor): Extension<emob_service::Actor>,
    input: Result<Json<crate::actions::Request>, JsonRejection>,
) -> Result<Json<crate::actions::Preview>, StatusCode> {
    let input = payload(input)?;
    lock(&shared.book)
        .preview(&actor, input, time::OffsetDateTime::now_utc())
        .map(Json)
        .map_err(refused)
}

async fn confirm<J: Journal>(
    State(shared): State<Shared<J>>,
    Extension(actor): Extension<emob_service::Actor>,
    input: Result<Json<crate::actions::Confirmation>, JsonRejection>,
) -> Result<Json<crate::actions::ResultView>, StatusCode> {
    let input = payload(input)?;
    lock(&shared.book)
        .confirm(
            &actor,
            &input.operation,
            input.reference.as_ref(),
            time::OffsetDateTime::now_utc(),
        )
        .map(Json)
        .map_err(refused)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrolRequest {
    point: EvseId,
    plan: String,
    stock: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrolConfirmation {
    operation: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgreementLookup {
    agreement: String,
}

#[derive(Serialize)]
struct Options {
    offers: Vec<crate::enrol::Selection>,
}

async fn options<J: Journal>(State(shared): State<Shared<J>>) -> Json<Options> {
    Json(Options {
        offers: shared.catalog.options().to_vec(),
    })
}

async fn enrol_preview<J: Journal>(
    State(shared): State<Shared<J>>,
    Extension(actor): Extension<emob_service::Actor>,
    input: Result<Json<EnrolRequest>, JsonRejection>,
) -> Result<Json<crate::enrol::Review>, StatusCode> {
    let input = payload(input)?;
    let uid = shared.catalog.stock_uid(&input.stock)?;
    let offer = shared
        .catalog
        .current(&input.point, &input.plan, &shared.clock)
        .await?;
    lock(&shared.book)
        .review_agreement(&actor, input.stock, uid, offer, (shared.clock)())
        .map(Json)
}

async fn enrol_confirm<J: Journal>(
    State(shared): State<Shared<J>>,
    Extension(actor): Extension<emob_service::Actor>,
    input: Result<Json<EnrolConfirmation>, JsonRejection>,
) -> Result<Json<crate::Agreement>, StatusCode> {
    let input = payload(input)?;
    let review = {
        let book = lock(&shared.book);
        if let Some(agreement) = book.own_receipt(&actor, &input.operation)? {
            return Ok(Json(agreement));
        }
        book.pending_agreement(&actor, &input.operation, (shared.clock)())?
            .clone()
    };
    let offer = shared
        .catalog
        .current(
            &review.offer.terms.point,
            &review.offer.terms.code,
            &shared.clock,
        )
        .await?;
    let uid = shared.catalog.stock_uid(&review.stock)?;
    // Recheck operation, time, current terms and remaining stock under the lock
    // after I/O. Two simultaneous confirms can append at most one agreement.
    lock(&shared.book)
        .enrol(&actor, &input.operation, uid, &offer, (shared.clock)())
        .map(Json)
}

async fn agreement_lookup<J: Journal>(
    State(shared): State<Shared<J>>,
    input: Result<Json<AgreementLookup>, JsonRejection>,
) -> Result<Json<crate::Agreement>, StatusCode> {
    let query = payload(input)?;
    lock(&shared.book)
        .receipt(&query.agreement)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn private(mut response: Response) -> Response {
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    response
}

/// Serve station authorisation behind the service token and provider reads behind operator tokens.
pub fn router<J: Journal>(shared: Shared<J>, operators: Operators) -> axum::Router {
    let view =
        axum::Router::new()
            .route(
                "/v1/empd",
                get(|State(shared): State<Shared<J>>| async move {
                    Json(View::of(&lock(&shared.book)))
                }),
            )
            .route(
                "/v1/empd/cards/lookup",
                post(lookup::<J>).layer(DefaultBodyLimit::max(2048)),
            )
            .route("/v1/empd/contracts/options", get(options::<J>))
            .route(
                "/v1/empd/contracts/lookup",
                post(agreement_lookup::<J>).layer(DefaultBodyLimit::max(2048)),
            )
            .with_state(shared.clone());
    let actions = axum::Router::new()
        .route("/v1/empd/cards/preview", post(preview::<J>))
        .route("/v1/empd/cards/confirm", post(confirm::<J>))
        .route("/v1/empd/contracts/preview", post(enrol_preview::<J>))
        .route("/v1/empd/contracts/confirm", post(enrol_confirm::<J>))
        .layer(DefaultBodyLimit::max(2048))
        .with_state(shared.clone());
    axum::Router::new()
        .route(
            "/v1/authorize",
            post(authorize::<J>).layer(DefaultBodyLimit::max(2048)),
        )
        .with_state(shared)
        .merge(
            emob_service::serve::operator_only(view, operators.clone())
                .merge(emob_service::serve::operator_write(actions, operators)),
        )
        .layer(axum::middleware::map_response(private))
}
