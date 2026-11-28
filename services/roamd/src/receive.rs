//! `POST /v1/deliveries` — a `de.emob.cdr.issued` `CloudEvent` from `csmsd`,
//! partner whose contract it was charged under.
//!
//! The same order as `billd`'s receiver: capped before read (SR-038), signed
//! and fresh before parsed (SR-029, SR-030), journalled before answered. A
//! record this node already consigned is a duplicate and changes nothing — the
//! delivery id is the record's key.
//!
//! The document is built **here**, from the estate's own station location and
//! the signed records as the station sent them, so the bytes the partner
//! receives are the bytes the journal holds.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use emob_core::{ContractId, PartyId};
use emob_roam::ocpi::cdr::Context;
use emob_roam::{RoamingToken, SignedPayload, TokenType};
use emob_service::journal::Journal;
use emob_service::page::{self, Page, Refused};
use emob_service::webhook;
use emob_service::{Actor, Operators};
use serde::{Deserialize, Serialize};

use crate::{Delivery, Node};

/// The largest delivery a receiver reads.
pub const BODY_LIMIT: usize = 1024 * 1024;

/// How far a delivery's signature may be from this clock.
pub const TOLERANCE: time::Duration = time::Duration::minutes(5);

/// The token, as `empd` presented it.
#[derive(Debug, Deserialize)]
pub struct Token {
    /// The issuer.
    pub issuer: PartyId,
    /// The uid.
    pub uid: String,
    /// What was presented.
    pub token_type: TokenType,
    /// The contract.
    pub contract_id: ContractId,
}

/// The parts of a `de.emob.cdr.issued` event's data `roamd` reads.
#[derive(Debug, Deserialize)]
pub struct Body {
    /// The record.
    pub cdr: emob_cdr::Cdr,
    /// Who pays.
    pub token: Token,
    /// The signed OCMF records, verbatim.
    pub signed: Vec<String>,
    /// The station that ran it.
    pub station: String,
    /// The point and its place (spec 021), so a station registered after
    /// this daemon started still has a location.
    #[serde(default)]
    pub point: Option<crate::daemon::Point>,
}

/// What the routes share.
pub struct Shared<J: Journal> {
    /// The node.
    pub node: Arc<Mutex<Node<J>>>,
    /// The secret deliveries are signed with.
    pub secret: webhook::Secret,
    /// Each estate station's OCPI location, by identity.
    pub locations: Arc<BTreeMap<String, ocpi_kit::v2_3_0::cdrs::CdrLocation>>,
    /// Deliveries refused before the journal.
    pub refused: Arc<AtomicU64>,
}

impl<J: Journal> Clone for Shared<J> {
    fn clone(&self) -> Self {
        Self {
            node: Arc::clone(&self.node),
            secret: self.secret.clone(),
            locations: Arc::clone(&self.locations),
            refused: Arc::clone(&self.refused),
        }
    }
}

fn lock<T>(held: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    held.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn refuse(status: StatusCode, reason: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "reason": reason.into() }))).into_response()
}

/// The signed records as OCPI carries them: what each is, and the bytes.
#[must_use]
pub fn payloads(signed: &[String]) -> Vec<SignedPayload> {
    let last = signed.len().saturating_sub(1);
    signed
        .iter()
        .enumerate()
        .map(|(index, record)| SignedPayload {
            nature: match index {
                0 => "Start",
                i if i == last => "End",
                _ => "Intermediate",
            }
            .to_owned(),
            plain_data: record.clone(),
            signed_data: record.clone(),
        })
        .collect()
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
    let body: Body = match emob_service::cloudevent::open_cdr(&body, &id) {
        Ok(Some(data)) => data,
        Ok(None) => {
            tracing::info!(%id, "a test delivery was taken and not booked");
            return (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "test": true })),
            )
                .into_response();
        }
        Err(refusal) => return refuse(StatusCode::UNPROCESSABLE_ENTITY, refusal.to_string()),
    };
    if body.cdr.key.to_string() != id {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "the delivery id {id} is not the record's key {}",
                body.cdr.key
            ),
        );
    }
    if lock(&shared.node)
        .state()
        .consignment(&body.cdr.key)
        .is_some()
    {
        return Json(serde_json::json!({ "duplicate": true })).into_response();
    }
    let carried = body
        .point
        .as_ref()
        .and_then(|point| crate::daemon::point_location("point", point).ok());
    let Some(location) = carried.or_else(|| shared.locations.get(&body.station).cloned()) else {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{} is not a station in the estate", body.station),
        );
    };
    // Through the validating constructor: the contract's check digit is
    // verified here because here is where it routes money.
    let token = match RoamingToken::new(
        body.token.issuer,
        body.token.uid,
        body.token.token_type,
        body.token.contract_id,
    ) {
        Ok(token) => token,
        Err(error) => return refuse(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
    };
    let context = Context {
        token: &token,
        location,
        signed: payloads(&body.signed),
        public_key: None,
        last_updated: now,
    };
    match lock(&shared.node).consign(&Actor::service("csmsd"), &body.cdr, &token, Some(&context)) {
        Ok(_) => Json(serde_json::json!({ "duplicate": false })).into_response(),
        Err(crate::NodeError::Journal { source, .. }) => {
            refuse(StatusCode::SERVICE_UNAVAILABLE, source.to_string())
        }
        Err(error) => refuse(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()),
    }
}

/// What a caller may ask of the view: nothing else is accepted (SR-089).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    /// Rows, `1..=200`, default 50.
    pub limit: Option<usize>,
    /// Consignments strictly older than this position.
    pub consignments: Option<u64>,
}

/// The bounded read model (spec 007): the partners, and the newest
/// consignments a partner has not accepted.
#[derive(Debug, Serialize)]
pub struct Today {
    /// The rows this answer was cut at.
    pub limit: usize,
    /// The partners and whether each has registered — the estate's list.
    pub partners: Vec<PartnerView>,
    /// Deliveries refused before the journal since this process started.
    pub refused_deliveries: u64,
    /// Consignments no partner has accepted, newest first.
    pub consignments: Page<ConsignmentView>,
}

/// One partner.
#[derive(Debug, Serialize)]
pub struct PartnerView {
    /// The party.
    pub party: String,
    /// Whether its platform has registered.
    pub registered: bool,
}

/// One consignment not yet accepted.
#[derive(Debug, Serialize)]
pub struct ConsignmentView {
    /// The record.
    pub key: String,
    /// To whom.
    pub recipient: String,
    /// `pending` or `refused`.
    pub delivery: &'static str,
    /// The refusal's reason, when refused.
    pub reason: Option<String>,
}

impl Today {
    /// The bounded view of one node: a range of the unaccepted index, never a
    /// walk over every record sent (SR-091).
    ///
    /// # Errors
    ///
    /// [`Refused`] for a `limit` outside `1..=200` (SR-088).
    pub fn of<J: Journal>(node: &Node<J>, refused: u64, params: &Params) -> Result<Self, Refused> {
        let limit = page::limit(params.limit)?;
        let state = node.state();
        let consignments = Page::newest(
            state
                .unaccepted_before(params.consignments.unwrap_or(u64::MAX))
                .map(|(position, consignment)| {
                    let (delivery, reason) = match &consignment.delivery {
                        Delivery::Refused { reason, .. } => ("refused", Some(reason.clone())),
                        Delivery::Pending | Delivery::Accepted { .. } => ("pending", None),
                    };
                    (
                        position,
                        ConsignmentView {
                            key: consignment.key.to_string(),
                            recipient: consignment.recipient().to_string(),
                            delivery,
                            reason,
                        },
                    )
                }),
            state.unaccepted_count() as u64,
            limit,
        );
        Ok(Self {
            limit,
            partners: state
                .registry()
                .partners()
                .iter()
                .map(|partner| PartnerView {
                    party: partner.party.to_string(),
                    registered: state.peers().for_party(&partner.party).is_some(),
                })
                .collect(),
            refused_deliveries: refused,
            consignments,
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
        &lock(&shared.node),
        shared.refused.load(Ordering::Relaxed),
        &params,
    ) {
        Ok(view) => Json(view).into_response(),
        Err(refused) => refused.into_response(),
    }
}

/// The receiver and the view.
pub fn router<J: Journal>(shared: Shared<J>, operators: Operators) -> axum::Router {
    let view = axum::Router::new()
        .route("/v1/roamd/today", get(today::<J>))
        .with_state(shared.clone());
    axum::Router::new()
        .route(
            "/v1/deliveries",
            post(deliveries::<J>).layer(DefaultBodyLimit::disable()),
        )
        .with_state(shared)
        .merge(emob_service::serve::operator_only(view, operators))
}
