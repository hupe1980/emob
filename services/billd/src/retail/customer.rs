//! Customer reads use current grants from the agreement owner on every request.
use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::{Json, Router, routing::post};
use base64::Engine as _;
use emob_core::{EvseId, PartyId};
use emob_service::journal::Journal;
use serde::{Deserialize, Serialize};

use super::Document;
use super::http::{Shared, xml_attachment};

const PAGE_SIZE: usize = 25;

#[derive(Deserialize)]
struct Access {
    subject: String,
    accounts: BTreeSet<String>,
    agreements: Vec<AgreementGrant>,
}

#[derive(Deserialize)]
struct AgreementGrant {
    agreement: String,
    emaid: String,
    reference: String,
    point: EvseId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryRequest {
    #[serde(default)]
    page: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentRequest {
    number: String,
}

#[derive(Serialize)]
struct History {
    invoices: Vec<Document>,
    receipts: Vec<ChargingSession>,
    next_page: Option<u32>,
}

#[derive(Serialize)]
struct ChargingSession {
    point: EvseId,
    kwh: String,
    #[serde(with = "time::serde::rfc3339")]
    ended_at: time::OffsetDateTime,
    agreement: String,
    source: String,
    receipt: String,
}

fn trusted_receipt<'a>(url: &'a str, prefix: Option<&str>) -> Option<&'a str> {
    let token = url.strip_prefix(prefix?)?;
    if token.len() != 22 {
        return None;
    }
    let mut decoded = [0_u8; 16];
    (base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode_slice(token, &mut decoded)
        .ok()?
        == 16)
        .then_some(url)
}

fn page<T>(rows: impl Iterator<Item = T>, offset: usize) -> (Vec<T>, bool) {
    let mut rows = rows.skip(offset);
    let items = rows.by_ref().take(PAGE_SIZE).collect();
    (items, rows.next().is_some())
}

impl Access {
    fn history(
        &self,
        state: &crate::Billd,
        provider: &PartyId,
        prefix: Option<&str>,
        index: u32,
    ) -> History {
        let offset = (index as usize).saturating_mul(PAGE_SIZE);
        let ledger = &state.retail;
        let numbers = self
            .accounts
            .iter()
            .filter_map(|account| {
                ledger
                    .account_documents
                    .get(&(provider.clone(), account.clone()))
            })
            .flat_map(|numbers| numbers.iter().rev());
        let (numbers, more_docs) = page(numbers, offset);
        let invoices = numbers
            .into_iter()
            .filter_map(|n| ledger.document_row(n))
            .collect();
        let rows = self.agreements.iter().flat_map(|grant| {
            ledger
                .agreement_receipts
                .get(&grant.agreement)
                .into_iter()
                .flat_map(|keys| keys.iter().rev())
                .filter_map(move |(_, key)| {
                    let received = state.received.get(key)?;
                    let binding = received.retail.as_ref()?;
                    if &received.partner != provider
                        || binding.emaid != grant.emaid
                        || binding.agreement != grant.agreement
                        || binding.reference != grant.reference
                        || received.cdr.evse_id != grant.point
                    {
                        return None;
                    }
                    let link = trusted_receipt(received.receipt.as_deref()?, prefix)?;
                    Some((grant, key, link, &received.cdr))
                })
        });
        let (rows, more_receipts) = page(rows, offset);
        let receipts = rows
            .into_iter()
            .map(|(grant, key, link, cdr)| ChargingSession {
                point: cdr.evse_id.clone(),
                kwh: cdr.total_energy.kwh().normalize().to_string(),
                ended_at: cdr.ended_at.to_offset(time::UtcOffset::UTC),
                agreement: grant.agreement.clone(),
                source: key.clone(),
                receipt: link.to_owned(),
            })
            .collect();
        History {
            invoices,
            receipts,
            next_page: (more_docs || more_receipts).then(|| index.saturating_add(1)),
        }
    }
}

async fn authorize<J: Journal>(
    owner: &Shared<J>,
    headers: &HeaderMap,
) -> Result<Access, StatusCode> {
    let mut access: Access = owner
        .provider_json(&owner.customer_url, headers, &serde_json::json!({}))
        .await
        .map_err(|status| {
            if status == StatusCode::UNAUTHORIZED {
                status
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            }
        })?;
    if access.subject.is_empty() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    access
        .agreements
        .sort_unstable_by(|a, b| a.agreement.cmp(&b.agreement));
    Ok(access)
}

async fn history<J: Journal>(
    State(owner): State<Arc<Shared<J>>>,
    headers: HeaderMap,
    Json(req): Json<HistoryRequest>,
) -> Result<Json<History>, StatusCode> {
    let access = authorize(&owner, &headers).await?;
    let books = owner.books();
    if books.is_halted() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(Json(access.history(
        books.state(),
        &owner.provider,
        owner.receipt_prefix.as_deref(),
        req.page,
    )))
}

fn granted_document<'a>(
    owner: &emob_core::PartyId,
    ledger: &'a super::Ledger,
    access: &Access,
    number: &str,
) -> Result<(&'a super::Issue, &'a str), StatusCode> {
    ledger
        .archived(number)
        .filter(|(issue, _)| {
            &issue.provider == owner && access.accounts.contains(&issue.summary.account)
        })
        .ok_or(StatusCode::NOT_FOUND)
}

#[derive(Serialize)]
struct BalanceBasis {
    replacement: bool,
    number: String,
    account: String,
    currency: emob_core::Currency,
    amount_minor: i64,
    credited_minor: i64,
}

async fn basis<J: Journal>(
    State(owner): State<Arc<Shared<J>>>,
    headers: HeaderMap,
    Json(req): Json<DocumentRequest>,
) -> Result<Json<BalanceBasis>, StatusCode> {
    let access = authorize(&owner, &headers).await?;
    let books = owner.books();
    if books.is_halted() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let ledger = &books.state().retail;
    let (issue, _) = granted_document(&owner.provider, ledger, &access, &req.number)?;
    if req.number != issue.document.number || !ledger.current_debit(issue) {
        return Err(StatusCode::CONFLICT);
    }
    let currency = issue.document.currency;
    Ok(Json(BalanceBasis {
        replacement: issue.summary.supersedes.is_some(),
        credited_minor: ledger.adjustments.gross(&req.number),
        number: req.number,
        account: issue.summary.account.clone(),
        currency,
        amount_minor: super::feed::minor_units(currency, issue.document.payable_total().amount())
            .ok_or(StatusCode::SERVICE_UNAVAILABLE)?,
    }))
}

async fn document<J: Journal>(
    State(owner): State<Arc<Shared<J>>>,
    headers: HeaderMap,
    Json(req): Json<DocumentRequest>,
) -> Result<Response, StatusCode> {
    let access = authorize(&owner, &headers).await?;
    let books = owner.books();
    if books.is_halted() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let (_, xml) = granted_document(&owner.provider, &books.state().retail, &access, &req.number)?;
    Ok(xml_attachment(xml.to_owned()))
}

pub(super) fn routes<J: Journal>(owner: Arc<Shared<J>>) -> Router {
    Router::new()
        .route("/v1/billd/customer/history", post(history::<J>))
        .route("/v1/billd/customer/balance-basis", post(basis::<J>))
        .route("/v1/billd/customer/document", post(document::<J>))
        .with_state(owner)
}
