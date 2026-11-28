//! Read the private provider's agreement snapshot before taking the books lock.
use std::sync::{Arc, Mutex};

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json, Router, routing::post};
use emob_billing::{Specification, Syntax};
use emob_core::PartyId;
use emob_service::estate::Billing;
use emob_service::journal::Journal;
use emob_service::{Actor, Clock, Operators};
use serde::Deserialize;

use super::calculation::prepare;
use super::{Issue, Period, Profile, Summary};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub account: String,
    pub period: String,
    review: Option<String>,
    number: Option<String>,
}

pub(super) struct Shared<J: Journal> {
    books: Arc<Mutex<crate::Books<J>>>,
    pub(super) clock: Clock,
    pub(super) key: Arc<emob_service::vault::DataKey>,
    pub(super) case_url: reqwest::Url,
    client: reqwest::Client,
    url: reqwest::Url,
    pub(super) customer_url: reqwest::Url,
    pub(super) receipt_prefix: Option<String>,
    pub(super) provider: PartyId,
    seller: Option<Billing>,
}

impl<J: Journal> Shared<J> {
    pub(super) fn books(&self) -> std::sync::MutexGuard<'_, crate::Books<J>> {
        self.books
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    async fn profile(&self, account: &str, headers: &HeaderMap) -> Result<Profile, StatusCode> {
        self.provider_json(&self.url, headers, &serde_json::json!({"account":account}))
            .await
    }

    pub(super) async fn provider_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &reqwest::Url,
        headers: &HeaderMap,
        body: &serde_json::Value,
    ) -> Result<T, StatusCode> {
        let bearer = headers
            .get(header::AUTHORIZATION)
            .filter(|v| v.to_str().is_ok_and(|v| v.starts_with("Bearer ")))
            .ok_or(StatusCode::UNAUTHORIZED)?;
        let mut response = self
            .client
            .post(url.clone())
            .header(header::AUTHORIZATION, bearer)
            .json(body)
            .send()
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        match response.status() {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(StatusCode::UNAUTHORIZED);
            }
            StatusCode::NOT_FOUND => return Err(StatusCode::NOT_FOUND),
            _ => return Err(StatusCode::SERVICE_UNAVAILABLE),
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        {
            if bytes.len().saturating_add(chunk.len()) > 4 * 1024 * 1024 {
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
    }
    fn validate(req: &Request) -> Result<Period, StatusCode> {
        if !emob_service::estate::valid_account_id(&req.account) {
            return Err(StatusCode::BAD_REQUEST);
        }
        Period::parse(&req.period).ok_or(StatusCode::BAD_REQUEST)
    }
}

fn confirmed_close<J: Journal>(
    owner: &Shared<J>,
    actor: &Actor,
    input: &Request,
) -> Result<Option<Summary>, StatusCode> {
    let token = input
        .review
        .as_ref()
        .filter(|t| t.len() == 64)
        .ok_or(StatusCode::BAD_REQUEST)?;
    // Lost replies remain recoverable while the provider is unavailable.
    Ok(owner
        .books()
        .state()
        .retail
        .issues
        .values()
        .find(|i| {
            i.provider == owner.provider
                && i.summary.account == input.account
                && i.summary.period == input.period
                && &i.confirmed_by == actor
                && i.confirmation == *token
        })
        .map(|i| i.summary.clone()))
}

async fn evaluate<J: Journal>(
    owner: &Shared<J>,
    actor: &Actor,
    headers: &HeaderMap,
    input: &Request,
    close: bool,
) -> Result<Json<Summary>, StatusCode> {
    let period = Shared::<J>::validate(input)?;
    if input.number.is_some() || (!close && input.review.is_some()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if close && let Some(issued) = confirmed_close(owner, actor, input)? {
        return Ok(Json(issued));
    }
    if period.next.midnight().assume_utc() > (owner.clock)() {
        return Err(StatusCode::CONFLICT);
    }
    let seller = owner
        .seller
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let profile = owner.profile(&input.account, headers).await?;
    if profile.account.id != input.account {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let mut books = owner.books();
    if books.is_halted() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let previous = books
        .state()
        .retail
        .latest(&owner.provider, &input.account, &input.period);
    if let Some(held) = previous.and_then(|prior| credited_month(&books.state().retail, prior)) {
        return if close {
            Err(StatusCode::CONFLICT)
        } else {
            Ok(Json(held))
        };
    }
    let mut candidate = prepare(
        books.state(),
        &owner.provider,
        seller,
        input,
        period,
        &profile,
    );
    if let Some(prior) = previous {
        if close
            && &prior.confirmed_by == actor
            && input.review.as_ref() == Some(&prior.confirmation)
        {
            return Ok(Json(prior.summary.clone()));
        }
        if candidate.content == prior.content {
            if close {
                return Err(StatusCode::CONFLICT);
            }
            return Ok(Json(prior.summary.clone()));
        }
        if books
            .state()
            .retail
            .adjustments
            .has_credits(&prior.document.number)
        {
            if close {
                return Err(StatusCode::CONFLICT);
            }
            candidate.summary.state = "held".into();
            candidate
                .summary
                .hold(&prior.document.number, "line_credit_issued");
            return Ok(Json(candidate.summary));
        }
        candidate.summary.supersedes = Some(prior.document.number.clone());
    }
    let ready = candidate.summary.state == "ready";
    let confirmation = candidate.confirmation(actor, previous.map(|i| i.document.number.as_str()));
    if !close {
        if ready {
            candidate.summary.review = Some(confirmation);
        }
        return Ok(Json(candidate.summary));
    }
    if !ready || input.review.as_ref() != Some(&confirmation) {
        return Err(StatusCode::CONFLICT);
    }
    let now = (owner.clock)().to_offset(time::UtcOffset::UTC).date();
    let issue = issue(
        &books.state().retail,
        previous,
        &owner.provider,
        now,
        candidate,
        actor,
        confirmation,
    )?;
    let summary = issue.summary.clone();
    books
        .issue_retail(actor, issue)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(Json(summary))
}

fn credited_month(ledger: &super::Ledger, prior: &Issue) -> Option<Summary> {
    let credit = ledger.credited_invoices.get(&prior.document.number)?;
    let mut summary = prior.summary.clone();
    "held".clone_into(&mut summary.state);
    summary.review = None;
    summary.credit = Some(credit.clone());
    summary.hold(&prior.document.number, "withdrawal_credit_issued");
    Some(summary)
}

fn issue(
    ledger: &super::Ledger,
    previous: Option<&Issue>,
    provider: &PartyId,
    now: time::Date,
    mut candidate: super::calculation::Candidate,
    actor: &Actor,
    confirmation: String,
) -> Result<Issue, StatusCode> {
    let mut invoice = candidate.doc.ok_or(StatusCode::CONFLICT)?;

    let reversal = previous
        .map(|i| {
            i.document.cancellation(
                ledger.number(provider, 1),
                now,
                "Superseded by a reviewed retail-period correction.",
            )
        })
        .transpose()
        .map_err(|_| StatusCode::CONFLICT)?;
    let number = ledger.number(provider, if reversal.is_some() { 2 } else { 1 });
    invoice.number.clone_from(&number);
    invoice.issued_on = now;
    let reversal = if let Some(credit) = reversal {
        invoice
            .notes
            .push(emob_billing::invoice::DocumentNote::on_the_document(
                format!(
                    "Replaces invoice {} reversed by credit note {}.",
                    credit.cancels.as_ref().ok_or(StatusCode::CONFLICT)?.number,
                    credit.number
                ),
            ));
        let xml = emob_billing::en16931::write(&credit, Specification::Core, Syntax::Ubl)
            .map_err(|_| StatusCode::CONFLICT)?
            .value;
        candidate.summary.credit = Some(credit.number.clone());
        Some(super::ArchivedCredit {
            number: credit.number,
            xml,
        })
    } else {
        None
    };
    let xml = emob_billing::en16931::write(&invoice, Specification::Core, Syntax::Ubl)
        .map_err(|_| StatusCode::CONFLICT)?
        .value;
    candidate.summary.number = Some(number);
    "issued".clone_into(&mut candidate.summary.state);
    Ok(Issue {
        provider: provider.clone(),
        document: invoice,
        xml,
        reversal,
        summary: candidate.summary,
        content: candidate.content,
        confirmation,
        confirmed_by: actor.clone(),
    })
}

async fn review<J: Journal>(
    State(owner): State<Arc<Shared<J>>>,
    Extension(actor): Extension<Actor>,
    headers: HeaderMap,
    Json(req): Json<Request>,
) -> Result<Json<Summary>, StatusCode> {
    evaluate(&owner, &actor, &headers, &req, false).await
}
async fn close<J: Journal>(
    State(owner): State<Arc<Shared<J>>>,
    Extension(actor): Extension<Actor>,
    headers: HeaderMap,
    Json(req): Json<Request>,
) -> Result<Json<Summary>, StatusCode> {
    evaluate(&owner, &actor, &headers, &req, true).await
}

fn stored<'a>(
    ledger: &'a super::Ledger,
    provider: &PartyId,
    input: &Request,
) -> Option<(&'a Issue, &'a str)> {
    let latest = ledger.latest(provider, &input.account, &input.period)?;
    let Some(number) = &input.number else {
        return Some((latest, &latest.xml));
    };
    ledger.archived(number).filter(|(issue, _)| {
        issue.provider == *provider
            && issue.summary.account == input.account
            && issue.summary.period == input.period
    })
}

async fn lookup<J: Journal>(
    State(owner): State<Arc<Shared<J>>>,
    Json(input): Json<Request>,
) -> Result<Json<Summary>, StatusCode> {
    Shared::<J>::validate(&input)?;
    let books = owner.books();
    let (issue, _) =
        stored(&books.state().retail, &owner.provider, &input).ok_or(StatusCode::NOT_FOUND)?;
    if input
        .number
        .as_ref()
        .is_some_and(|n| n != &issue.document.number)
    {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(issue.summary.clone()))
}

async fn document<J: Journal>(
    State(owner): State<Arc<Shared<J>>>,
    Json(input): Json<Request>,
) -> Result<Response, StatusCode> {
    Shared::<J>::validate(&input)?;
    let xml = {
        let books = owner.books();
        stored(&books.state().retail, &owner.provider, &input)
            .ok_or(StatusCode::NOT_FOUND)?
            .1
            .to_owned()
    };
    Ok(xml_attachment(xml))
}

pub(super) fn xml_attachment(xml: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/xml; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"invoice.xml\"",
            ),
        ],
        xml,
    )
        .into_response()
}

async fn private(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "private, no-store".parse().expect("static header"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        "no-referrer".parse().expect("static header"),
    );
    response
}

pub(crate) fn router<J: Journal>(
    books: Arc<Mutex<crate::Books<J>>>,
    estate: &crate::daemon::Estate,
    clock: Clock,
    operators: Operators,
    key: Arc<emob_service::vault::DataKey>,
) -> anyhow::Result<Router> {
    let mut url = reqwest::Url::parse(&estate.services.empd)?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.has_host()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.query().is_none(),
        "invalid provider service URL"
    );
    let mut case_url = url.clone();
    case_url
        .path_segments_mut()
        .map_err(|()| anyhow::anyhow!("invalid provider service URL"))?
        .pop_if_empty()
        .extend(["v1", "empd", "contracts", "withdrawal", "lookup"]);
    let mut customer_url = url.clone();
    customer_url
        .path_segments_mut()
        .map_err(|()| anyhow::anyhow!("invalid provider service URL"))?
        .pop_if_empty()
        .extend(["v1", "empd", "customer", "me"]);
    let receipt_prefix = estate
        .receipts
        .as_ref()
        .map(|r| {
            emob_service::estate::validate_public_origin(&r.public_url)?;
            Ok::<_, anyhow::Error>(format!("{}/r/", r.public_url.trim_end_matches('/')))
        })
        .transpose()?;
    url.path_segments_mut()
        .map_err(|()| anyhow::anyhow!("invalid provider service URL"))?
        .pop_if_empty()
        .extend(["v1", "empd", "contracts", "billing-agreements"]);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()?;
    let shared = Arc::new(Shared {
        books,
        clock,
        client,
        url,
        customer_url,
        key,
        case_url,
        receipt_prefix,
        provider: estate.parties.provider.clone(),
        seller: estate.billing.retail_seller.clone(),
    });
    let adjustments = super::adjustment::routes(Arc::clone(&shared), operators.clone());
    let feed = super::feed::routes(Arc::clone(&shared), operators.clone());
    let withdrawals = super::withdrawal::routes(Arc::clone(&shared), operators.clone());
    let customers = super::customer::routes(Arc::clone(&shared));
    let reads = Router::new()
        .route("/v1/billd/retail/review", post(review::<J>))
        .route("/v1/billd/retail/lookup", post(lookup::<J>))
        .with_state(Arc::clone(&shared));
    let archive = Router::new()
        .route("/v1/billd/retail/document", post(document::<J>))
        .with_state(Arc::clone(&shared));
    let writes = Router::new()
        .route("/v1/billd/retail/close", post(close::<J>))
        .with_state(shared);
    Ok(emob_service::serve::operator_only(reads, operators.clone())
        .merge(emob_service::serve::finance_only(
            archive,
            operators.clone(),
        ))
        .merge(emob_service::serve::operator_write(writes, operators))
        .merge(adjustments)
        .merge(customers)
        .merge(withdrawals)
        .merge(feed)
        .layer(DefaultBodyLimit::max(2048))
        .layer(axum::middleware::map_response(private)))
}
