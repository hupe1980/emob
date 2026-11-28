//! A finance writer credits one archived UBL line. Decisions and XML replay together.
use std::collections::BTreeMap;
use std::sync::Arc;

use axum::{Extension, Json, Router, extract::State, http::StatusCode, routing::post};
use emob_billing::{Specification, Syntax, invoice::InvoiceLine};
use emob_core::{Currency, PartyId};
use emob_service::{Actor, Operators, journal::Journal};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::{Issue, Ledger, Line, feed::minor_units, http::Shared};

type Result<T> = std::result::Result<T, StatusCode>;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reason {
    BillingError,
    ServiceDispute,
    Goodwill,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Choice {
    number: String,
    agreement: String,
    line_id: String,
    net_minor: i64,
    reason: Reason,
}

impl Choice {
    fn original_line<'a>(&self, issue: &'a Issue) -> Result<(&'a InvoiceLine, &'a Line)> {
        let group = issue
            .summary
            .lines
            .iter()
            .find(|l| l.agreement == self.agreement && l.document_lines.contains(&self.line_id))
            .ok_or(StatusCode::NOT_FOUND)?;
        let line = issue
            .document
            .lines
            .iter()
            .find(|l| l.id == self.line_id)
            .ok_or(StatusCode::NOT_FOUND)?;
        Ok((line, group))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    #[serde(flatten)]
    choice: Choice,
    review: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Ready,
    Issued,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Decision {
    #[serde(flatten)]
    choice: Choice,
    account: String,
    period: String,
    currency: Currency,
    tax_minor: i64,
    pub(super) gross_minor: i64,
    remaining_minor: i64,
    vat_rate: String,
    state: Stage,
    review: String,
    credit: Option<String>,
}

/// A sealed journal fact containing the confirmed line credit and its immutable XML.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LineCredit {
    actor: Actor,
    pub(super) receipt: Decision,
    pub(super) xml: String,
}

impl LineCredit {
    pub(super) fn original(&self) -> &str {
        &self.receipt.choice.number
    }
}

#[derive(Debug, Default)]
struct CreditedLine {
    net: i64,
    tax: i64,
    last_credit: Option<String>,
}

#[derive(Debug, Default)]
struct CreditedInvoice {
    gross: i64,
    lines: BTreeMap<String, CreditedLine>,
}

#[derive(Debug, Default)]
pub(super) struct Adjustments {
    documents: BTreeMap<String, LineCredit>,
    by_review: BTreeMap<String, String>,
    invoices: BTreeMap<String, CreditedInvoice>,
}

impl Adjustments {
    fn replay(&self, actor: &Actor, req: &Command) -> Result<Option<&Decision>> {
        let Some(credit) = req
            .review
            .as_ref()
            .and_then(|r| self.by_review.get(r))
            .and_then(|n| self.get(n))
        else {
            return Ok(None);
        };
        if &credit.actor != actor || credit.receipt.choice != req.choice {
            return Err(StatusCode::CONFLICT);
        }
        Ok(Some(&credit.receipt))
    }

    pub(super) fn has_credits(&self, number: &str) -> bool {
        self.invoices.contains_key(number)
    }

    pub(super) fn gross(&self, original: &str) -> i64 {
        self.invoices.get(original).map_or(0, |total| total.gross)
    }

    fn line(&self, original: &str, line_id: &str) -> Option<&CreditedLine> {
        self.invoices.get(original)?.lines.get(line_id)
    }

    pub(super) fn get(&self, number: &str) -> Option<&LineCredit> {
        self.documents.get(number)
    }
}

impl Ledger {
    pub(crate) fn apply_line_credit(&mut self, credit: LineCredit) {
        let decision = &credit.receipt;
        let choice = &decision.choice;
        let number = decision
            .credit
            .as_ref()
            .expect("issued credit has a number");
        let issue = &self.issues[&choice.number];
        self.document_numbers.push(number.clone());
        self.account_documents
            .entry((issue.provider.clone(), issue.summary.account.clone()))
            .or_default()
            .push(number.clone());
        let totals = self
            .adjustments
            .invoices
            .entry(choice.number.clone())
            .or_default();
        totals.gross += decision.gross_minor;
        let line = totals.lines.entry(choice.line_id.clone()).or_default();
        line.net += choice.net_minor;
        line.tax += decision.tax_minor;
        line.last_credit = Some(number.clone());
        self.adjustments
            .by_review
            .insert(decision.review.clone(), number.clone());
        self.adjustments.documents.insert(number.clone(), credit);
    }

    pub(super) fn current_debit(&self, issue: &Issue) -> bool {
        let number = &issue.document.number;
        !self.credited_invoices.contains_key(number)
            && self
                .latest(
                    &issue.provider,
                    &issue.summary.account,
                    &issue.summary.period,
                )
                .is_some_and(|current| current.document.number == *number)
    }

    fn attributable(&self, provider: &PartyId, original: &str, agreement: &str) -> Result<&Issue> {
        if original.is_empty()
            || original.len() > 96
            || !agreement
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || agreement.len() != 64
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        self.issues
            .get(original)
            .filter(|i| {
                i.provider == *provider && i.summary.lines.iter().any(|l| l.agreement == agreement)
            })
            .ok_or(StatusCode::NOT_FOUND)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    number: String,
    agreement: String,
    #[serde(default)]
    page: u32,
}

#[derive(Serialize)]
struct Selectable {
    line_id: String,
    source: String,
    kind: String,
    net_minor: i64,
    remaining_minor: i64,
    vat_rate: String,
    last_credit: Option<String>,
}

fn selectable(
    issue: &Issue,
    line: &InvoiceLine,
    group: &Line,
    ledger: &Ledger,
) -> Result<Selectable> {
    let net = minor_units(issue.document.currency, line.net).ok_or(StatusCode::CONFLICT)?;
    let prior = ledger.adjustments.line(&issue.document.number, &line.id);
    Ok(Selectable {
        line_id: line.id.clone(),
        source: group.source.clone(),
        kind: group.kind.clone(),
        net_minor: net,
        remaining_minor: net - prior.map_or(0, |t| t.net),
        vat_rate: line
            .vat_rate
            .ok_or(StatusCode::CONFLICT)?
            .normalize()
            .to_string(),
        last_credit: prior.and_then(|t| t.last_credit.clone()),
    })
}

#[derive(Serialize)]
struct Scope {
    number: String,
    agreement: String,
    account: String,
    period: String,
    currency: Currency,
    state: &'static str,
    lines: Vec<Selectable>,
    next_page: Option<u32>,
}

async fn lookup<J: Journal>(
    State(owner): State<Arc<Shared<J>>>,
    Json(req): Json<Lookup>,
) -> Result<Json<Scope>> {
    let books = owner.books();
    if books.is_halted() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let ledger = &books.state().retail;
    let issue = ledger.attributable(&owner.provider, &req.number, &req.agreement)?;
    let groups: BTreeMap<_, _> = issue
        .summary
        .lines
        .iter()
        .filter(|l| l.agreement == req.agreement)
        .flat_map(|l| l.document_lines.iter().map(move |id| (id, l)))
        .collect();
    let mut matches = issue
        .document
        .lines
        .iter()
        .filter_map(|line| Some((line, *groups.get(&line.id)?)))
        .skip((req.page as usize).saturating_mul(50));
    let lines = matches
        .by_ref()
        .take(50)
        .map(|(line, group)| selectable(issue, line, group, ledger))
        .collect::<Result<_>>()?;
    let next_page = matches.next().and_then(|_| req.page.checked_add(1));
    Ok(Json(Scope {
        number: req.number,
        agreement: req.agreement,
        account: issue.summary.account.clone(),
        period: issue.summary.period.clone(),
        currency: issue.document.currency,
        state: if ledger.current_debit(issue) {
            "available"
        } else {
            "closed"
        },
        lines,
        next_page,
    }))
}

fn evaluate<J: Journal>(
    owner: &Shared<J>,
    actor: &Actor,
    req: Command,
    confirm: bool,
) -> Result<Json<Decision>> {
    if req.choice.net_minor <= 0
        || req.choice.line_id.is_empty()
        || req.choice.line_id.len() > 64
        || confirm != req.review.is_some()
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut books = owner.books();
    if books.is_halted() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let ledger = &books.state().retail;
    let adjustments = &ledger.adjustments;
    if let Some(receipt) = adjustments.replay(actor, &req)? {
        return Ok(Json(receipt.clone()));
    }
    let choice = req.choice;
    let issue = ledger.attributable(&owner.provider, &choice.number, &choice.agreement)?;
    let (line, group) = choice.original_line(issue)?;
    if !ledger.current_debit(issue) {
        return Err(StatusCode::CONFLICT);
    }
    let available = selectable(issue, line, group, ledger)?;
    let previous = adjustments.line(&choice.number, &choice.line_id);
    let gross_before = adjustments.gross(&choice.number);
    let number = ledger.number(&owner.provider, 1);
    let doc = issue
        .document
        .line_credit(
            &number,
            (owner.clock)().to_offset(time::UtcOffset::UTC).date(),
            &line.id,
            Decimal::new(choice.net_minor, 2),
            Decimal::new(previous.map_or(0, |t| t.net), 2),
            Decimal::new(previous.map_or(0, |t| t.tax), 2),
        )
        .map_err(|_| StatusCode::CONFLICT)?;
    let gross =
        minor_units(doc.currency, doc.payable_total().amount()).ok_or(StatusCode::CONFLICT)?;
    let tax = minor_units(doc.currency, doc.tax_total().amount()).ok_or(StatusCode::CONFLICT)?;
    let original = minor_units(doc.currency, issue.document.payable_total().amount())
        .ok_or(StatusCode::CONFLICT)?;
    if gross > original - gross_before {
        return Err(StatusCode::CONFLICT);
    }
    let snapshot = serde_json::to_vec(&(
        actor,
        &issue.provider,
        &issue.content,
        &choice,
        &available.last_credit,
        gross_before,
        tax,
        gross,
    ))
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let review = owner
        .key
        .digest("billd/line-credit/v1", &snapshot)
        .hex()
        .to_owned();
    if confirm && req.review.as_ref() != Some(&review) {
        return Err(StatusCode::CONFLICT);
    }
    let decision = Decision {
        remaining_minor: available.remaining_minor - choice.net_minor,
        vat_rate: available.vat_rate,
        choice,
        account: issue.summary.account.clone(),
        period: issue.summary.period.clone(),
        currency: doc.currency,
        tax_minor: tax,
        gross_minor: gross,
        state: if confirm { Stage::Issued } else { Stage::Ready },
        review,
        credit: confirm.then(|| number.clone()),
    };
    if confirm {
        let xml = emob_billing::en16931::write(&doc, Specification::Core, Syntax::Ubl)
            .map_err(|_| StatusCode::CONFLICT)?
            .value;
        books
            .line_credit(
                actor,
                LineCredit {
                    actor: actor.clone(),
                    receipt: decision.clone(),
                    xml,
                },
                &number,
            )
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    }
    Ok(Json(decision))
}

async fn review<J: Journal>(
    State(owner): State<Arc<Shared<J>>>,
    Extension(actor): Extension<Actor>,
    Json(req): Json<Command>,
) -> Result<Json<Decision>> {
    evaluate(&owner, &actor, req, false)
}

async fn confirm<J: Journal>(
    State(owner): State<Arc<Shared<J>>>,
    Extension(actor): Extension<Actor>,
    Json(req): Json<Command>,
) -> Result<Json<Decision>> {
    evaluate(&owner, &actor, req, true)
}

pub(super) fn routes<J: Journal>(owner: Arc<Shared<J>>, operators: Operators) -> Router {
    let routes = Router::new()
        .route("/v1/billd/adjustment/lookup", post(lookup::<J>))
        .route("/v1/billd/adjustment/review", post(review::<J>))
        .route("/v1/billd/adjustment/confirm", post(confirm::<J>))
        .with_state(owner);
    emob_service::serve::operator_write(
        emob_service::serve::finance_only(routes, operators.clone()),
        operators,
    )
}
