//! Finance staff read open items and confirm allocations at their owning ledger.
pub(super) mod adjustment;
mod offset;
mod views;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Form, Multipart, Query, Request, State};
use axum::http::Method;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Extension, Router};
use emob_core::Currency;
use emob_service::authority::OperatorAccess;
use emob_service::ui::Lang;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::fetch::Why;
use crate::investigation::Params;
use crate::{Page, language, stations};

const STATEMENT_LIMIT: usize = 64 * 1024;

pub(super) fn routes() -> Router<Arc<Page>> {
    Router::new()
        .route("/receivables", get(index))
        .route("/receivables/balance", post(balance))
        .route("/receivables/import", post(import))
        .route(
            "/receivables/upload",
            post(upload).layer(DefaultBodyLimit::max(STATEMENT_LIMIT + 4096)),
        )
        .route("/receivables/review", post(review))
        .route("/receivables/confirm", post(confirm))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .route_layer(middleware::from_fn_with_state(
            Workspace::Receivables,
            authorize,
        ))
        .merge(offset::routes().route_layer(middleware::from_fn_with_state(
            Workspace::Offsets,
            authorize,
        )))
        .merge(
            adjustment::routes().route_layer(middleware::from_fn_with_state(
                Workspace::LineCredits,
                authorize,
            )),
        )
}

#[derive(Clone)]
struct Finance {
    token: String,
    lang: Lang,
}

#[derive(Clone, Copy)]
enum Workspace {
    Receivables,
    LineCredits,
    Offsets,
}

impl Workspace {
    fn problem(self, lang: Lang, why: Why) -> Response {
        match self {
            Self::Receivables => views::problem(lang, why),
            Self::Offsets => offset::problem(lang, why),
            Self::LineCredits => adjustment::problem(lang, why, None),
        }
    }
}

async fn authorize(State(workspace): State<Workspace>, mut req: Request, next: Next) -> Response {
    let query = Query::<Params>::try_from_uri(req.uri()).ok();
    let lang = language(
        query.as_ref().and_then(|q| q.lang.as_deref()),
        req.headers(),
    );
    let granted = req.extensions().get::<OperatorAccess>().is_some_and(|a| {
        a.can_finance() && (matches!(workspace, Workspace::Receivables) || a.can_write())
    });
    if !granted || (req.method() == Method::POST && !stations::same_origin(req.headers())) {
        return workspace.problem(lang, Why::Refused);
    }
    let Some(token) = stations::operator_token(req.headers()) else {
        return workspace.problem(lang, Why::Refused);
    };
    req.extensions_mut().insert(Finance { token, lang });
    let response = next.run(req).await;
    if response
        .headers()
        .contains_key(axum::http::header::CACHE_CONTROL)
    {
        return response;
    }
    // Extractor errors can quote invalid fields. Keep private references out of error pages.
    if matches!(response.status().as_u16(), 400 | 413 | 415 | 422) {
        return workspace.problem(lang, Why::Status(response.status().as_u16()));
    }
    emob_service::ui::hardened(response, emob_service::ui::CSP)
}

fn endpoint(page: &Page, action: &str) -> Result<String, Why> {
    let base = page.accountingd.as_deref().ok_or(Why::NoAnswer)?;
    Ok(format!(
        "{}/v1/accountingd/{action}",
        base.trim_end_matches('/')
    ))
}

impl Finance {
    async fn ask<T: DeserializeOwned>(
        &self,
        page: &Page,
        action: &str,
        input: &Value,
    ) -> Result<T, Why> {
        let url = endpoint(page, action)?;
        page.reader.operator_action(&url, &self.token, input).await
    }
}

#[derive(Deserialize, Serialize, PartialEq)]
struct Scope {
    account: String,
    currency: Currency,
}

#[derive(Deserialize, Serialize)]
struct AccountPage {
    #[serde(flatten)]
    scope: Scope,
    #[serde(default)]
    page: u32,
}

#[derive(Deserialize)]
struct Statement {
    statement: String,
}

#[derive(Deserialize, Serialize)]
struct Selection {
    #[serde(flatten)]
    scope: Scope,
    source: String,
    reference: String,
    number: String,
}

#[derive(Deserialize)]
struct Draft {
    #[serde(flatten)]
    selection: Selection,
    amount: String,
}

#[derive(Deserialize, Serialize)]
struct Allocation {
    #[serde(flatten)]
    selection: Selection,
    amount_minor: i64,
}

#[derive(Deserialize, Serialize)]
struct Decision {
    #[serde(flatten)]
    selection: Selection,
    amount_minor: i64,
    review: String,
}

#[derive(Deserialize)]
struct Preview {
    review: String,
    open_after_minor: i64,
    unapplied_after_minor: i64,
}

async fn index(State(desk): State<Arc<Page>>, Extension(auth): Extension<Finance>) -> Response {
    if desk.accountingd.is_none() {
        return views::problem(auth.lang, Why::NoAnswer);
    }
    views::index(auth.lang)
}

async fn balance(
    State(desk): State<Arc<Page>>,
    Extension(auth): Extension<Finance>,
    Extension(access): Extension<OperatorAccess>,
    Form(query): Form<AccountPage>,
) -> Response {
    match auth
        .ask::<views::Balance>(&desk, "balance", &json!(query))
        .await
    {
        Ok(result) if result.matches(&query.scope) => {
            result.render(auth.lang, query.page, access.can_write())
        }
        Ok(_) => views::problem(auth.lang, Why::Unreadable),
        Err(why) => views::problem(auth.lang, why),
    }
}

async fn import(
    State(desk): State<Arc<Page>>,
    Extension(auth): Extension<Finance>,
    Form(input): Form<Statement>,
) -> Response {
    submit_statement(&desk, &auth, input.statement.as_bytes()).await
}

async fn upload(
    State(desk): State<Arc<Page>>,
    Extension(auth): Extension<Finance>,
    mut form: Multipart,
) -> Response {
    let file = match form.next_field().await {
        Ok(Some(file)) if file.name() == Some("file") => file,
        Err(e) => return views::problem(auth.lang, Why::Status(e.status().as_u16())),
        _ => return views::problem(auth.lang, Why::Status(400)),
    };
    let bytes = match file.bytes().await {
        Ok(data) => data,
        Err(e) => return views::problem(auth.lang, Why::Status(e.status().as_u16())),
    };
    match form.next_field().await {
        Ok(None) => submit_statement(&desk, &auth, &bytes).await,
        Err(e) => views::problem(auth.lang, Why::Status(e.status().as_u16())),
        Ok(Some(_)) => views::problem(auth.lang, Why::Status(400)),
    }
}

async fn submit_statement(desk: &Page, auth: &Finance, bytes: &[u8]) -> Response {
    if bytes.len() > STATEMENT_LIMIT {
        return views::problem(auth.lang, Why::Status(413));
    }
    let Ok(scope) = serde_json::from_slice::<Scope>(bytes) else {
        return views::problem(auth.lang, Why::Status(400));
    };
    let url = match endpoint(desk, "import") {
        Ok(url) => url,
        Err(why) => return views::problem(auth.lang, why),
    };
    match desk
        .reader
        .statement::<views::Imported>(&url, &auth.token, bytes)
        .await
    {
        Ok(result) => result.render(auth.lang, &scope),
        Err(why) => views::problem(auth.lang, why),
    }
}

async fn review(
    State(desk): State<Arc<Page>>,
    Extension(auth): Extension<Finance>,
    Form(draft): Form<Draft>,
) -> Response {
    let Some(amount_minor) = minor_units(&draft.amount, draft.selection.scope.currency) else {
        return views::edit(auth.lang, &draft);
    };
    let allocation = Allocation {
        selection: draft.selection,
        amount_minor,
    };
    match auth
        .ask::<Preview>(&desk, "review", &json!(allocation))
        .await
    {
        Ok(result) => views::decision(auth.lang, &allocation, &result, false),
        Err(why) => views::problem(auth.lang, why),
    }
}

async fn confirm(
    State(desk): State<Arc<Page>>,
    Extension(auth): Extension<Finance>,
    Form(input): Form<Decision>,
) -> Response {
    match auth.ask::<Preview>(&desk, "confirm", &json!(input)).await {
        Ok(result) if result.review == input.review => {
            let allocation = Allocation {
                selection: input.selection,
                amount_minor: input.amount_minor,
            };
            views::decision(auth.lang, &allocation, &result, true)
        }
        Ok(_) => views::problem(auth.lang, Why::Unreadable),
        Err(why) => views::problem(auth.lang, why),
    }
}

// Both languages accept either decimal separator. Grouping and rounding are ambiguous in a payment.
fn minor_units(raw: &str, currency: Currency) -> Option<i64> {
    let raw = raw.trim();
    let (whole, fraction) = raw.split_once(['.', ',']).unwrap_or((raw, ""));
    let digits = currency.minor_unit_digits();
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > usize::try_from(digits).ok()?
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || (fraction.is_empty() && whole != raw)
    {
        return None;
    }
    let mut amount = whole.parse::<i64>().ok()?.checked_mul(10_i64.pow(digits))?;
    if !fraction.is_empty() {
        let pad = digits.checked_sub(u32::try_from(fraction.len()).ok()?)?;
        amount = amount.checked_add(fraction.parse::<i64>().ok()?.checked_mul(10_i64.pow(pad))?)?;
    }
    (amount > 0).then_some(amount)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_native_confirmation_form_preserves_the_selected_minor_units() {
        use axum::{body::Body, extract::FromRequest as _};
        let body = "account=FLEET-A&currency=EUR&source=LOCAL-BANK&reference=ROW.1&number=INV-1&amount_minor=6050&review=test-review";
        let req = Request::post("/receivables/confirm")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap();
        let Form(input) = Form::<Decision>::from_request(req, &()).await.unwrap();
        assert_eq!(
            json!(input),
            json!({"account":"FLEET-A","currency":"EUR","source":"LOCAL-BANK","reference":"ROW.1","number":"INV-1","amount_minor":6050,"review":"test-review"})
        );
    }

    #[test]
    fn allocation_input_uses_exact_currency_units_without_rounding() {
        let kwd = Currency::new("KWD").unwrap();
        let jpy = Currency::new("JPY").unwrap();
        assert_eq!(minor_units("60,5", Currency::EUR), Some(6050));
        assert_eq!(minor_units("0.005", kwd), Some(5));
        assert_eq!(minor_units("12", jpy), Some(12));
        assert_eq!(minor_units("0.5", jpy), None);
        for invalid in [
            "60.001",
            ".5",
            "2.",
            "-2",
            "0",
            "2e1",
            "1,200.10",
            "92233720368547758.08",
        ] {
            assert_eq!(minor_units(invalid, Currency::EUR), None);
        }
    }
}
