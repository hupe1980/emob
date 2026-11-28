//! A payer can inspect the cash applied to a single currently authorized invoice.
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use emob_core::Currency;
use serde::{Deserialize, Serialize};

use crate::api::{Owner, bearer};
use crate::ledger::{BAD, OFFLINE, Result};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    number: String,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Basis {
    #[serde(skip_serializing)]
    pub replacement: bool,
    pub number: String,
    pub account: String,
    pub currency: Currency,
    pub amount_minor: i64,
    pub credited_minor: i64,
}

#[derive(Serialize)]
pub(crate) struct Balance {
    #[serde(flatten)]
    basis: Basis,
    paid_minor: i64,
    incoming_minor: i64,
    outgoing_minor: i64,
    due_minor: i64,
}

pub(crate) async fn balance(
    State(owner): State<Arc<Owner>>,
    headers: HeaderMap,
    Json(input): Json<Request>,
) -> Result<Json<Balance>> {
    if input.number.is_empty() || input.number.len() > 96 {
        return Err(BAD);
    }
    let endpoint = owner
        .feed
        .join("/v1/billd/customer/balance-basis")
        .map_err(|_| OFFLINE)?;
    let mut response = owner
        .client
        .post(endpoint)
        .header(header::AUTHORIZATION, bearer(&headers)?)
        .json(&input)
        .timeout(Duration::from_secs(4))
        .send()
        .await
        .map_err(|_| OFFLINE)?;
    match response.status() {
        StatusCode::OK => (),
        status @ (StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND | StatusCode::CONFLICT) => {
            return Err(status);
        }
        _ => return Err(OFFLINE),
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| OFFLINE)? {
        if chunk.len() > 2048 - bytes.len() {
            return Err(OFFLINE);
        }
        bytes.extend_from_slice(&chunk);
    }
    let basis: Basis = serde_json::from_slice(&bytes).map_err(|_| OFFLINE)?;
    if basis.number != input.number || !(0..=basis.amount_minor).contains(&basis.credited_minor) {
        return Err(OFFLINE);
    }
    let book = owner.book()?;
    book.live()?;
    let (paid_minor, incoming_minor, outgoing_minor) = book.state.booked_effects(&basis)?;
    let due_minor = crate::ledger::signed_due(
        basis.amount_minor - basis.credited_minor,
        paid_minor,
        incoming_minor,
        outgoing_minor,
    )?;
    Ok(Json(Balance {
        basis,
        paid_minor,
        incoming_minor,
        outgoing_minor,
        due_minor,
    }))
}
