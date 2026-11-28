//! Agreement-scoped evidence and separately reviewed full credit of an issued invoice.
use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use emob_billing::{Specification, Syntax};
use emob_core::{Currency, PartyId};
use emob_service::{Actor, Operators, authority::OperatorAccess, journal::Journal};
use serde::{Deserialize, Serialize};

use super::{Issue, Ledger, http::Shared};

/// An immutable credit linked to the approved agreement decision and its original invoice.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WithdrawalCredit {
    pub(crate) number: String,
    pub(super) original: String,
    agreement: String,
    operation: String,
    review: String,
    actor: Actor,
    pub(super) xml: String,
}

impl Ledger {
    pub(crate) fn apply_credit(&mut self, credit: WithdrawalCredit) {
        // Only issued facts can be credited. Both events are folded in journal order.
        let original = &self.issues[&credit.original];
        self.document_numbers.push(credit.number.clone());
        self.account_documents
            .entry((original.provider.clone(), original.summary.account.clone()))
            .or_default()
            .push(credit.number.clone());
        self.credited_invoices
            .insert(credit.original.clone(), credit.number.clone());
        self.withdrawals.insert(credit.number.clone(), credit);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    agreement: String,
    number: String,
    #[serde(default)]
    page: u32,
    review: Option<String>,
}

fn handle(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Serialize)]
struct Line {
    source: String,
    kind: String,
    gross: String,
}

#[derive(Serialize)]
struct Evidence {
    account: String,
    period: String,
    agreement: String,
    number: String,
    currency: Currency,
    payable: String,
    state: &'static str,
    lines: Vec<Line>,
    next_page: Option<u32>,
    review: Option<String>,
    credit: Option<String>,
}

type Result<T> = std::result::Result<T, StatusCode>;

impl Selection {
    fn already_confirmed(&self, ledger: &Ledger, actor: &Actor) -> bool {
        ledger
            .credited_invoices
            .get(&self.number)
            .and_then(|n| ledger.withdrawals.get(n))
            .is_some_and(|c| {
                &c.actor == actor
                    && self.review.as_ref() == Some(&c.review)
                    && c.agreement == self.agreement
            })
    }

    fn evidence(&self, ledger: &Ledger, issuer: &PartyId) -> Result<Evidence> {
        if !handle(&self.agreement) || self.number.is_empty() || self.number.len() > 96 {
            return Err(StatusCode::BAD_REQUEST);
        }
        let issue = ledger
            .issues
            .get(&self.number)
            .filter(|i| &i.provider == issuer)
            .ok_or(StatusCode::NOT_FOUND)?;
        let mut selected = issue
            .summary
            .lines
            .iter()
            .filter(|l| l.agreement == self.agreement)
            .peekable();
        if selected.peek().is_none() {
            return Err(StatusCode::NOT_FOUND);
        }
        let offset = (self.page as usize).saturating_mul(50);
        let mut selected = selected.skip(offset);
        let lines = selected
            .by_ref()
            .take(50)
            .map(|l| Line {
                source: l.source.clone(),
                kind: l.kind.clone(),
                gross: l.gross.clone(),
            })
            .collect();
        let next_page = selected.next().and_then(|_| self.page.checked_add(1));
        let credit = ledger.credited_invoices.get(&self.number).cloned();
        let summary = &issue.summary;
        let current = ledger.latest(issuer, &summary.account, &summary.period);
        let state = if credit.is_some() {
            "credited"
        } else if ledger.adjustments.has_credits(&self.number) {
            "line_adjusted"
        } else if current.is_none_or(|i| i.document.number != self.number) {
            "superseded"
        } else if summary.lines.iter().any(|l| l.agreement != self.agreement) {
            "mixed"
        } else {
            "evidence"
        };
        Ok(Evidence {
            account: summary.account.clone(),
            period: summary.period.clone(),
            agreement: self.agreement.clone(),
            number: self.number.clone(),
            currency: summary.currency,
            payable: summary.payable.clone(),
            lines,
            next_page,
            state,
            review: None,
            credit,
        })
    }
}

#[derive(Deserialize)]
struct Case {
    agreement: String,
    account: String,
    stage: String,
    decision: Option<Decision>,
}

#[derive(Deserialize)]
struct Decision {
    billing: String,
    operation: String,
}

struct Approval {
    account: String,
    operation: String,
}

impl<J: Journal> Shared<J> {
    async fn approval(&self, req: &Selection, headers: &HeaderMap) -> Result<Approval> {
        let case: Case = self
            .provider_json(
                &self.case_url,
                headers,
                &serde_json::json!({"agreement":req.agreement}),
            )
            .await?;
        let decision = case
            .decision
            .filter(|d| {
                case.stage == "approved"
                    && case.agreement == req.agreement
                    && d.billing == "review_required"
                    && handle(&d.operation)
            })
            .ok_or(StatusCode::CONFLICT)?;
        Ok(Approval {
            account: case.account,
            operation: decision.operation,
        })
    }
}

async fn evidence<J: Journal>(
    State(owner): State<Arc<Shared<J>>>,
    Json(req): Json<Selection>,
) -> Result<Json<Evidence>> {
    if req.review.is_some() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let books = owner.books();
    if books.is_halted() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    req.evidence(&books.state().retail, &owner.provider)
        .map(Json)
}

fn xml(issue: &Issue, number: &str, day: time::Date) -> Result<String> {
    let doc = issue
        .document
        .cancellation(
            number,
            day,
            "Credit following reviewed contract withdrawal.",
        )
        .map_err(|_| StatusCode::CONFLICT)?;
    emob_billing::en16931::write(&doc, Specification::Core, Syntax::Ubl)
        .map(|out| out.value)
        .map_err(|_| StatusCode::CONFLICT)
}

async fn evaluate<J: Journal>(
    owner: &Shared<J>,
    req: Selection,
    headers: HeaderMap,
    actor: Actor,
    access: OperatorAccess,
    confirm: bool,
) -> Result<Json<Evidence>> {
    if !access.can_finance() {
        return Err(StatusCode::FORBIDDEN);
    }
    if req.page != 0
        || (confirm && req.review.as_deref().is_none_or(|r| !handle(r)))
        || (!confirm && req.review.is_some())
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    {
        let books = owner.books();
        if books.is_halted() {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        let ledger = &books.state().retail;
        let result = req.evidence(ledger, &owner.provider)?;
        if result.state != "evidence" {
            return if !confirm || req.already_confirmed(ledger, &actor) {
                Ok(Json(result))
            } else {
                Err(StatusCode::CONFLICT)
            };
        }
    }
    let approval = owner.approval(&req, &headers).await?;
    let mut books = owner.books();
    if books.is_halted() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let ledger = &books.state().retail;
    let mut result = req.evidence(ledger, &owner.provider)?;
    if result.account != approval.account {
        return Err(StatusCode::CONFLICT);
    }
    if confirm && req.already_confirmed(ledger, &actor) {
        return Ok(Json(result));
    }
    if result.state != "evidence" {
        return Err(StatusCode::CONFLICT);
    }
    let issue = &ledger.issues[&result.number];
    let bytes = serde_json::to_vec(&(
        &actor,
        &issue.provider,
        &issue.content,
        &result.number,
        &req.agreement,
        &approval.operation,
    ))
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let review = owner
        .key
        .digest("billd/withdrawal-credit/v1", &bytes)
        .hex()
        .to_owned();
    if !confirm {
        result.state = "ready";
        result.review = Some(review);
        return Ok(Json(result));
    }
    if req.review.as_ref() != Some(&review) {
        return Err(StatusCode::CONFLICT);
    }
    let number = ledger.number(&owner.provider, 1);
    let credit = WithdrawalCredit {
        xml: xml(
            issue,
            &number,
            (owner.clock)().to_offset(time::UtcOffset::UTC).date(),
        )?,
        number: number.clone(),
        original: result.number.clone(),
        agreement: req.agreement,
        operation: approval.operation,
        actor: actor.clone(),
        review,
    };
    books
        .withdrawal_credit(&actor, credit)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    result.state = "credited";
    result.credit = Some(number);
    Ok(Json(result))
}

async fn review<J: Journal>(
    State(owner): State<Arc<Shared<J>>>,
    Extension(actor): Extension<Actor>,
    Extension(access): Extension<OperatorAccess>,
    headers: HeaderMap,
    Json(req): Json<Selection>,
) -> Result<Json<Evidence>> {
    evaluate(&owner, req, headers, actor, access, false).await
}
async fn confirm<J: Journal>(
    State(owner): State<Arc<Shared<J>>>,
    Extension(actor): Extension<Actor>,
    Extension(access): Extension<OperatorAccess>,
    headers: HeaderMap,
    Json(req): Json<Selection>,
) -> Result<Json<Evidence>> {
    evaluate(&owner, req, headers, actor, access, true).await
}

pub(super) fn routes<J: Journal>(owner: Arc<Shared<J>>, operators: Operators) -> Router {
    let routes = Router::new()
        .route("/v1/billd/withdrawal/evidence", post(evidence::<J>))
        .route("/v1/billd/withdrawal/review", post(review::<J>))
        .route("/v1/billd/withdrawal/confirm", post(confirm::<J>))
        .with_state(owner);
    emob_service::serve::operator_write(routes, operators)
}
