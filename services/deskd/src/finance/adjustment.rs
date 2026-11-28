//! Review a credit against one issued retail invoice line before sending it to billing.
mod views;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Form, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Extension, Router};
use emob_core::Currency;
use emob_service::ui::Lang;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::{Finance, minor_units};
use crate::fetch::Why;
use crate::investigation::{shell, valid_reference};
use crate::{Page, retail};

pub(super) use views::problem;
pub(crate) use views::{open_invoice, title};

pub(super) fn routes() -> Router<Arc<Page>> {
    Router::new()
        .route("/line-credits", get(index))
        .route("/line-credits/lookup", post(lookup))
        .route("/line-credits/review", post(review))
        .route("/line-credits/confirm", post(confirm))
        .layer(DefaultBodyLimit::max(2048))
}

#[derive(Deserialize, Serialize, PartialEq)]
pub(super) struct Invoice {
    number: String,
    agreement: String,
}

fn reference(value: &str) -> bool {
    !value.is_empty() && value.len() <= 96 && !value.chars().any(char::is_control)
}

impl Invoice {
    fn valid(&self) -> bool {
        reference(&self.number) && valid_reference(&self.agreement)
    }
}

#[derive(Deserialize, Serialize)]
struct Lookup {
    #[serde(flatten)]
    invoice: Invoice,
    #[serde(default)]
    page: u32,
}

#[derive(Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Reason {
    BillingError,
    ServiceDispute,
    Goodwill,
}

impl Reason {
    const fn code(&self) -> &'static str {
        match self {
            Self::BillingError => "billing_error",
            Self::ServiceDispute => "service_dispute",
            Self::Goodwill => "goodwill",
        }
    }

    fn label(&self, lang: Lang) -> &'static str {
        match self {
            Self::BillingError => lang.pick("Abrechnungsfehler", "Billing error"),
            Self::ServiceDispute => lang.pick("Beanstandete Leistung", "Service dispute"),
            Self::Goodwill => lang.pick("Kulanz", "Goodwill"),
        }
    }
}

#[derive(Deserialize, Serialize, PartialEq)]
struct Choice {
    #[serde(flatten)]
    invoice: Invoice,
    line_id: String,
    reason: Reason,
}

#[derive(Deserialize)]
struct Draft {
    #[serde(flatten)]
    choice: Choice,
    amount: String,
}

#[derive(Deserialize, Serialize)]
struct Confirmation {
    #[serde(flatten)]
    choice: Choice,
    net_minor: i64,
    review: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Eligibility {
    Available,
    Closed,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Monthly,
    Session,
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "line_id")]
    id: String,
    source: String,
    kind: Kind,
    net_minor: i64,
    remaining_minor: i64,
    vat_rate: Decimal,
    last_credit: Option<String>,
}

#[derive(Deserialize)]
struct Archive {
    #[serde(flatten)]
    invoice: Invoice,
    account: String,
    period: String,
    currency: Currency,
    state: Eligibility,
    lines: Vec<Line>,
    next_page: Option<u32>,
}

impl Archive {
    fn matches(&self, lookup: &Lookup) -> bool {
        self.invoice == lookup.invoice
            && retail::valid_account(&self.account)
            && retail::valid_period(&self.period)
            && self.currency == Currency::EUR
            && self.lines.len() <= 50
            && self.lines.iter().all(|l| {
                reference(&l.id)
                    && (0..=l.net_minor).contains(&l.remaining_minor)
                    && !l.vat_rate.is_sign_negative()
                    && l.last_credit.as_deref().is_none_or(reference)
            })
            && self
                .next_page
                .is_none_or(|next| Some(next) == lookup.page.checked_add(1))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Stage {
    Ready,
    Issued,
}

#[derive(Deserialize)]
struct Decision {
    #[serde(flatten)]
    choice: Choice,
    account: String,
    period: String,
    currency: Currency,
    net_minor: i64,
    tax_minor: i64,
    gross_minor: i64,
    remaining_minor: i64,
    vat_rate: Decimal,
    state: Stage,
    review: String,
    credit: Option<String>,
}

impl Decision {
    fn matches(&self, choice: &Choice, net: i64, review: Option<&str>) -> bool {
        &self.choice == choice
            && self.net_minor == net
            && net > 0
            && self.tax_minor >= 0
            && net.checked_add(self.tax_minor) == Some(self.gross_minor)
            && self.remaining_minor >= 0
            && !self.vat_rate.is_sign_negative()
            && self.currency == Currency::EUR
            && retail::valid_account(&self.account)
            && retail::valid_period(&self.period)
            && valid_reference(&self.review)
            && match (&self.state, review) {
                (Stage::Ready, None) => self.credit.is_none(),
                (Stage::Issued, Some(expected)) => {
                    self.review == expected && self.credit.as_deref().is_some_and(reference)
                }
                _ => false,
            }
    }
}

async fn index(Extension(auth): Extension<Finance>) -> Response {
    shell(auth.lang, title(auth.lang), &views::lookup_form(auth.lang))
}

async fn fetch<T: DeserializeOwned>(
    desk: &Page,
    auth: &Finance,
    action: &str,
    input: &Value,
) -> Result<T, Why> {
    let url = format!(
        "{}/v1/billd/adjustment/{action}",
        desk.billd.trim_end_matches('/')
    );
    desk.reader.operator_action(&url, &auth.token, input).await
}

async fn lookup(
    State(desk): State<Arc<Page>>,
    Extension(auth): Extension<Finance>,
    Form(input): Form<Lookup>,
) -> Response {
    if !input.invoice.valid() {
        return problem(auth.lang, Why::Status(400), None);
    }
    match fetch::<Archive>(&desk, &auth, "lookup", &json!(input)).await {
        Ok(out) if out.matches(&input) => views::lines(&out, input.page, auth.lang),
        Ok(_) => problem(auth.lang, Why::Unreadable, Some(&input.invoice)),
        Err(why) => problem(auth.lang, why, Some(&input.invoice)),
    }
}

async fn review(
    State(desk): State<Arc<Page>>,
    Extension(auth): Extension<Finance>,
    Form(input): Form<Draft>,
) -> Response {
    if !input.choice.invoice.valid() || !reference(&input.choice.line_id) {
        return problem(auth.lang, Why::Status(400), None);
    }
    let Some(amount) = minor_units(&input.amount, Currency::EUR) else {
        return views::edit_amount(&input, auth.lang);
    };
    let mut payload = json!(input.choice);
    payload["net_minor"] = json!(amount);
    match fetch::<Decision>(&desk, &auth, "review", &payload).await {
        Ok(out) if out.matches(&input.choice, amount, None) => views::decision(&out, auth.lang),
        Ok(_) => problem(auth.lang, Why::Unreadable, Some(&input.choice.invoice)),
        Err(why) => problem(auth.lang, why, Some(&input.choice.invoice)),
    }
}

async fn confirm(
    State(desk): State<Arc<Page>>,
    Extension(auth): Extension<Finance>,
    Form(input): Form<Confirmation>,
) -> Response {
    if !input.choice.invoice.valid()
        || !reference(&input.choice.line_id)
        || !valid_reference(&input.review)
        || input.net_minor <= 0
    {
        return problem(auth.lang, Why::Status(400), None);
    }
    match fetch::<Decision>(&desk, &auth, "confirm", &json!(input)).await {
        Ok(out) if out.matches(&input.choice, input.net_minor, Some(&input.review)) => {
            views::decision(&out, auth.lang)
        }
        Ok(_) => problem(auth.lang, Why::Unreadable, Some(&input.choice.invoice)),
        Err(why) => problem(auth.lang, why, Some(&input.choice.invoice)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        extract::{FromRequest as _, Request},
    };

    fn form(fields: &str) -> Request {
        Request::post("/line-credits/confirm")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(fields.to_owned()))
            .unwrap()
    }

    #[tokio::test]
    async fn only_the_reviewed_line_amount_and_matching_receipt_can_be_displayed_as_issued() {
        let agreement = "a".repeat(64);
        let operation = "e".repeat(64);
        let selection =
            format!("number=EM-1&agreement={agreement}&line_id=S.1&reason=service_dispute");
        let Form(draft) =
            Form::<Draft>::from_request(form(&format!("{selection}&amount=1%2C20")), &())
                .await
                .unwrap();
        let amount = minor_units(&draft.amount, Currency::EUR).unwrap();
        assert_eq!(amount, 120);
        let Form(confirmed) = Form::<Confirmation>::from_request(
            form(&format!(
                "{selection}&net_minor={amount}&review={operation}"
            )),
            &(),
        )
        .await
        .unwrap();
        let mut reply = json!(confirmed);
        let details = json!({"account":"PAYER-FLEET","period":"2026-11","currency":"EUR","tax_minor":23,"gross_minor":143,"remaining_minor":167,"vat_rate":"19","state":"issued","credit":"CR-1"});
        reply
            .as_object_mut()
            .unwrap()
            .extend(details.as_object().unwrap().clone());
        let accepted = |value| {
            serde_json::from_value::<Decision>(value).unwrap().matches(
                &draft.choice,
                amount,
                Some(&operation),
            )
        };
        assert!(accepted(reply.clone()));
        for (key, bad) in [
            ("line_id", json!("S.2")),
            ("review", json!("c".repeat(64))),
            ("gross_minor", json!(142)),
            ("reason", json!("goodwill")),
            ("credit", Value::Null),
            ("state", json!("ready")),
        ] {
            let mut other = reply.clone();
            other[key] = bad;
            assert!(!accepted(other), "{key}");
        }
    }
}
