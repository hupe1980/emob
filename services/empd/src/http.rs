//! `empd` over HTTP: the one question `csmsd` asks, and the operator's view.
//!
//! - `POST /v1/authorize` — behind the estate's **service** token (SR-033). An
//!   unauthenticated caller would turn the token store into an oracle for which
//!   cards are valid and whose contract they bill.
//! - `GET /v1/empd` — behind the **operator** tokens (SR-034), and without a
//!   single uid: the reference a session holds is what an operator needs.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use emob_core::{ContractId, PartyId};
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
}

fn answer(status: Status, reason: impl Into<String>) -> Answer {
    Answer {
        status,
        reason: reason.into(),
        token: None,
        reference: None,
    }
}

/// Answer one uid on one day — the rule, without the socket.
pub fn decide<J: Journal>(book: &Book<J>, uid: &str, on: time::Date) -> Answer {
    let reference = book.state().reference_for(uid);
    match book.authorize(&reference, on) {
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
        Err(error) => answer(Status::Invalid, error.to_string()),
    }
}

/// What the routes share.
pub struct Shared<J: Journal> {
    /// The book.
    pub book: Arc<Mutex<Book<J>>>,
    /// The token `csmsd` presents.
    pub service: Token,
}

impl<J: Journal> Clone for Shared<J> {
    fn clone(&self) -> Self {
        Self {
            book: Arc::clone(&self.book),
            service: self.service.clone(),
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
    Json(request): Json<Request>,
) -> Result<Json<Answer>, StatusCode> {
    let offered = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if !offered.is_some_and(|offered| shared.service.verify(offered)) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let today = time::OffsetDateTime::now_utc().date();
    Ok(Json(decide(&lock(&shared.book), &request.uid, today)))
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

/// Both routes: authorise behind the service token, the view behind the
/// operators.
pub fn router<J: Journal>(shared: Shared<J>, operators: Operators) -> axum::Router {
    let view =
        axum::Router::new()
            .route(
                "/v1/empd",
                get(|State(shared): State<Shared<J>>| async move {
                    Json(View::of(&lock(&shared.book)))
                }),
            )
            .with_state(shared.clone());
    axum::Router::new()
        .route("/v1/authorize", post(authorize::<J>))
        .with_state(shared)
        .merge(emob_service::serve::operator_only(view, operators))
}
