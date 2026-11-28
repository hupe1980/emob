//! A privacy operator reviews a single portal identity and reopens the provider's durable receipt.
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Form, Query, Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Extension, Router};
use emob_service::authority::OperatorAccess;
use emob_service::ui::{Lang, escape};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::fetch::Why;
use crate::investigation::{Params, moment, shell, valid_reference, with_status};
use crate::{Page, language, stations};

const PATH: &str = "/customer-erasure";

pub(super) fn routes() -> Router<Arc<Page>> {
    Router::new()
        .route(PATH, get(index))
        .route("/customer-erasure/status", post(status))
        .route("/customer-erasure/review", post(review))
        .route("/customer-erasure/confirm", post(confirm))
        .layer(DefaultBodyLimit::max(1024))
        .route_layer(middleware::from_fn(authorize))
}

#[derive(Clone)]
struct Privacy {
    token: String,
    lang: Lang,
}

async fn authorize(mut req: Request, next: Next) -> Response {
    let params = Query::<Params>::try_from_uri(req.uri()).ok();
    let lang = language(
        params.as_ref().and_then(|p| p.lang.as_deref()),
        req.headers(),
    );
    let granted = req
        .extensions()
        .get::<OperatorAccess>()
        .is_some_and(|access| access.can_privacy());
    if !granted || (req.method() == Method::POST && !stations::same_origin(req.headers())) {
        return problem(lang, Why::Refused);
    }
    let Some(token) = stations::operator_token(req.headers()) else {
        return problem(lang, Why::Refused);
    };
    req.extensions_mut().insert(Privacy { token, lang });
    let response = next.run(req).await;
    // Form rejection bodies may otherwise echo private input.
    if !response.headers().contains_key(header::CACHE_CONTROL)
        && matches!(response.status().as_u16(), 400 | 413 | 415 | 422)
    {
        return problem(lang, Why::Status(response.status().as_u16()));
    }
    response
}

fn title(lang: Lang) -> &'static str {
    lang.pick("Kundendaten löschen", "Customer erasure")
}

fn open_link(lang: Lang) -> String {
    format!(
        r#"<a class="btn ghost" href="{PATH}?lang={}">{}</a>"#,
        lang.tag(),
        lang.pick("Kundenkonto suchen", "Find a customer")
    )
}

async fn index(Extension(privacy): Extension<Privacy>) -> Response {
    let lang = privacy.lang;
    let body = format!(
        r#"<form class="card" method="post" action="{PATH}/status?lang={}" autocomplete="off">
<p><label>{}<br><input type="text" name="subject" maxlength="64" required spellcheck="false" autocapitalize="none"></label></p>
<button class="btn">{}</button></form>"#,
        lang.tag(),
        lang.pick("Portal-Kennung", "Customer sign-in ID"),
        lang.pick("Kundenkonto öffnen", "Open customer")
    );
    shell(lang, title(lang), &body)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Subject {
    subject: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    subject: String,
    operation: String,
}

#[derive(Deserialize)]
struct Outcome {
    #[serde(rename = "policy")]
    _policy: Policy,
    subject: String,
    accounts: u64,
    agreements: u64,
    #[serde(flatten)]
    state: Disposition,
}

// Each version represents the precise data classes shown on the review and receipt.
#[derive(Deserialize)]
enum Policy {
    #[serde(rename = "portal_access_v1")]
    PortalAccess,
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
enum Disposition {
    Active,
    Ready {
        operation: String,
    },
    Erased {
        operation: String,
        actor: emob_service::Actor,
        #[serde(with = "time::serde::rfc3339")]
        erased_at: time::OffsetDateTime,
    },
}

#[derive(Clone, Copy)]
enum Action<'a> {
    Status,
    Review,
    Confirm(&'a str),
}

impl Action<'_> {
    const fn endpoint(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Review => "review",
            Self::Confirm(_) => "confirm",
        }
    }

    fn accepts(self, response: &Disposition) -> bool {
        match (self, response) {
            (Self::Status, Disposition::Active) => true,
            (Self::Review, Disposition::Ready { operation }) => valid_reference(operation),
            (
                action,
                Disposition::Erased {
                    operation, actor, ..
                },
            ) => {
                valid_reference(operation)
                    && matches!(actor, emob_service::Actor::Operator(name) if !name.is_empty())
                    && !matches!(action, Self::Confirm(expected) if expected != operation)
            }
            _ => false,
        }
    }
}

async fn status(
    State(desk): State<Arc<Page>>,
    Extension(privacy): Extension<Privacy>,
    Form(input): Form<Subject>,
) -> Response {
    ask(
        &desk,
        &privacy,
        Action::Status,
        &input.subject,
        &json!(input),
    )
    .await
}

async fn review(
    State(desk): State<Arc<Page>>,
    Extension(privacy): Extension<Privacy>,
    Form(input): Form<Subject>,
) -> Response {
    ask(
        &desk,
        &privacy,
        Action::Review,
        &input.subject,
        &json!(input),
    )
    .await
}

async fn confirm(
    State(desk): State<Arc<Page>>,
    Extension(privacy): Extension<Privacy>,
    Form(input): Form<Confirmation>,
) -> Response {
    if !valid_reference(&input.operation) {
        return problem(privacy.lang, Why::Status(400));
    }
    ask(
        &desk,
        &privacy,
        Action::Confirm(&input.operation),
        &input.subject,
        &json!(input),
    )
    .await
}

async fn ask(
    desk: &Page,
    privacy: &Privacy,
    action: Action<'_>,
    subject: &str,
    body: &Value,
) -> Response {
    let url = format!(
        "{}/v1/empd/customer/erasure/{}",
        desk.empd.trim_end_matches('/'),
        action.endpoint()
    );
    match desk
        .reader
        .operator_action::<Outcome>(&url, &privacy.token, body)
        .await
    {
        Ok(result) if result.subject == subject && action.accepts(&result.state) => {
            result.render(privacy.lang, &desk.zone)
        }
        Ok(_) => problem(privacy.lang, Why::Unreadable),
        Err(why) => problem(privacy.lang, why),
    }
}

fn problem(lang: Lang, why: Why) -> Response {
    let (code, message) = match why {
        Why::Refused | Why::Status(403) => (StatusCode::FORBIDDEN, lang.pick("Diese Aktion erfordert einen Datenschutz-Zugang und ein Formular aus Heute.", "Use an account with privacy access and submit the form from Today.")),
        Why::Status(400 | 413 | 415 | 422) => (StatusCode::BAD_REQUEST, lang.pick("Prüfen Sie die Portal-Kennung und öffnen Sie die Vorschau erneut.", "Check the sign-in ID and reopen the review.")),
        Why::Status(404) => (StatusCode::NOT_FOUND, lang.pick("Unter dieser Portal-Kennung wurde kein Kundenkonto gefunden.", "No customer was found for this sign-in ID.")),
        Why::Status(409) => (StatusCode::CONFLICT, lang.pick("Diese Bestätigung ist nicht mehr gültig. Öffnen Sie das Kundenkonto und prüfen Sie den aktuellen Stand.", "This confirmation is no longer valid. Reopen the customer to review the current status.")),
        _ => (StatusCode::SERVICE_UNAVAILABLE, lang.pick("Der Anbieter kann den Löschstatus derzeit nicht bestätigen. Öffnen Sie das Kundenkonto erneut, sobald er erreichbar ist.", "The provider cannot confirm the erasure status. Reopen the customer when the service is available.")),
    };
    with_status(
        shell(
            lang,
            title(lang),
            &format!(r#"<p role="alert">{message}</p>{}"#, open_link(lang)),
        ),
        code,
    )
}

impl Outcome {
    fn render(&self, lang: Lang, zone: &emob_core::TimeZone) -> Response {
        let subject = escape(&self.subject);
        let counts = format!(
            r#"<dl class="kv"><dt>{}</dt><dd>{subject}</dd><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd></dl>"#,
            lang.pick("Portal-Kennung", "Customer sign-in ID"),
            lang.pick("Vertragsfreigaben", "Agreement grants"),
            self.agreements,
            lang.pick("Kontofreigaben", "Billing account grants"),
            self.accounts
        );
        let scope = format!(r#"<input type="hidden" name="subject" value="{subject}">"#);
        let tag = lang.tag();
        let (heading, content) = match &self.state {
            Disposition::Active => {
                let body = format!(
                    r#"<section class="card"><h2>{active}</h2>{counts}
<form method="post" action="{PATH}/review?lang={tag}">{scope}<button class="btn">{review}</button></form></section>"#,
                    active = lang.pick("Aktives Kundenkonto", "Active customer"),
                    review = lang.pick("Löschung prüfen", "Review erasure"),
                );
                (
                    lang.pick("Löschstatus des Kundenkontos", "Customer erasure status"),
                    body,
                )
            }
            Disposition::Ready { operation } => {
                let body = format!(
                    r#"<section class="card"><h2>{permanent}</h2>{counts}<p>{effect}</p></section>
{retained}<form class="card-controls" method="post" action="{PATH}/confirm?lang={tag}">{scope}
<input type="hidden" name="operation" value="{operation}"><button class="btn">{confirm}</button>{back}</form>"#,
                    permanent = lang.pick("Endgültige Löschung", "Permanent erasure"),
                    effect = lang.pick(
                        "Bestätigen Sie die Löschung nach Prüfung des Antrags gemäß Art. 17 DSGVO. Alle Portalsitzungen dieser Person enden. Die Kennung wird gesperrt und bisherige Portalzuordnungen werden auch bei einer Wiederherstellung unlesbar.",
                        "Confirm after assessing the request under GDPR Article 17. All portal sessions for this customer end. The sign-in ID is disabled and current and historical portal grants become unreadable, including after a restore.",
                    ),
                    retained = retention(lang),
                    operation = escape(operation),
                    confirm = lang.pick("Portalzugriff endgültig löschen", "Permanently erase portal access"),
                    back = open_link(lang),
                );
                (
                    lang.pick(
                        "Löschung des Portalzugriffs prüfen",
                        "Review customer erasure",
                    ),
                    body,
                )
            }
            Disposition::Erased {
                operation,
                actor,
                erased_at,
            } => {
                let body = format!(
                    r#"<section class="card"><h2>{erased}</h2>{counts}<dl class="kv">
<dt>{who}</dt><dd>{actor}</dd><dt>{when}</dt><dd>{at}</dd><dt>{reference}</dt><dd class="mono">{operation}</dd></dl></section>{retained}{back}"#,
                    erased = lang.pick("Portalzugriff gelöscht", "Portal access erased"),
                    who = lang.pick("Bestätigt von", "Confirmed by"),
                    actor = escape(&actor.to_string()),
                    when = lang.pick("Gelöscht am", "Erased at"),
                    at = moment(*erased_at, zone, lang),
                    reference = lang.pick("Belegreferenz", "Receipt reference"),
                    operation = escape(operation),
                    retained = retention(lang),
                    back = open_link(lang),
                );
                (lang.pick("Löschbeleg", "Erasure receipt"), body)
            }
        };
        shell(lang, heading, &content)
    }
}

fn retention(lang: Lang) -> String {
    format!(r#"<section class="card"><h2>{}</h2><dl class="kv"><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd></dl></section>"#,
        lang.pick("Aufbewahrte Nachweise", "Retained evidence"),
        lang.pick("Rechnungen und Zahlungen", "Invoices and payments"),
        lang.pick("Ausgestellte Rechnungen unterliegen der steuerlichen Aufbewahrung (§ 14b UStG, Art. 17 Abs. 3 Buchst. b DSGVO). Zugeordnete Zahlungen bleiben für die Buchführung erhalten.", "Issued invoices are retained for tax obligations (German VAT Act § 14b, GDPR Art. 17(3)(b)). Payment allocations remain in the accounting ledger."),
        lang.pick("Verträge und Ladevorgänge", "Agreements and charging records"),
        lang.pick("Der Anbieter behält Vertrags- und Ladenachweise für die Vertragsabwicklung und die Prüfung von Forderungen. Eine aktive Ladekarte bleibt nutzbar.", "The provider keeps agreement and charging evidence for contract administration and claims review. An active charging card remains usable."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_changed_erasure_policy_or_unrelated_receipt_cannot_authorize_the_form() {
        let operation = "e".repeat(64);
        let mut receipt = json!({
            "policy": "portal_access_v1", "subject": "test-subject", "state": "erased",
            "agreements": 1, "accounts": 1, "erased_at": "2026-11-27T08:00:00Z",
            "actor": "operator:Privacy", "operation": operation
        });
        let parsed: Outcome = serde_json::from_value(receipt.clone()).unwrap();
        assert!(Action::Confirm(&operation).accepts(&parsed.state));
        assert!(!Action::Confirm(&"a".repeat(64)).accepts(&parsed.state));
        receipt["actor"] = json!("service:billd");
        let parsed: Outcome = serde_json::from_value(receipt.clone()).unwrap();
        assert!(!Action::Status.accepts(&parsed.state));
        receipt["policy"] = json!("different_scope");
        assert!(serde_json::from_value::<Outcome>(receipt).is_err());
    }
}
