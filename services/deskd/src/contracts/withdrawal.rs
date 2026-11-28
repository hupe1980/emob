//! Staff handle a received withdrawal against the original agreement.
mod invoice;
mod views;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Form, Query, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Extension, Router};
use emob_service::authority::OperatorAccess;
use emob_service::ui::{Lang, escape};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::fetch::Why;
use crate::investigation::{Params, shell, valid_reference, with_status};
use crate::{Page, language, stations};

const PATH: &str = "/contracts/withdrawal";

pub(super) fn routes() -> Router<Arc<Page>> {
    Router::new()
        .route(PATH, get(index))
        .route("/contracts/withdrawal/lookup", post(lookup))
        .route("/contracts/withdrawal/intake", post(intake))
        .route("/contracts/withdrawal/review", post(review))
        .route("/contracts/withdrawal/confirm", post(confirm))
        .route("/contracts/withdrawal/evidence", post(invoice::evidence))
        .route("/contracts/withdrawal/credit-review", post(invoice::review))
        .route(
            "/contracts/withdrawal/credit-confirm",
            post(invoice::confirm),
        )
        .layer(DefaultBodyLimit::max(4096))
        .route_layer(middleware::from_fn(authorize))
}

#[derive(Clone)]
struct Staff {
    token: String,
    lang: Lang,
    finance: bool,
}

async fn authorize(mut req: Request, next: Next) -> Response {
    let params = Query::<Params>::try_from_uri(req.uri()).ok();
    let lang = language(
        params.as_ref().and_then(|p| p.lang.as_deref()),
        req.headers(),
    );
    let Some(access) = req.extensions().get::<OperatorAccess>().copied() else {
        return problem(lang, Why::Refused, None);
    };
    if !access.can_write()
        || (req.method() == Method::POST && !stations::same_origin(req.headers()))
    {
        return problem(lang, Why::Refused, None);
    }
    let Some(token) = stations::operator_token(req.headers()) else {
        return problem(lang, Why::Refused, None);
    };
    req.extensions_mut().insert(Staff {
        token,
        lang,
        finance: access.can_finance(),
    });
    let response = next.run(req).await;
    if !response
        .headers()
        .contains_key(axum::http::header::CACHE_CONTROL)
        && matches!(response.status().as_u16(), 400 | 413 | 415 | 422)
    {
        return problem(lang, Why::Status(response.status().as_u16()), None);
    }
    response
}

impl Staff {
    async fn provider<T: DeserializeOwned>(
        &self,
        desk: &Page,
        endpoint: &str,
        input: &Value,
    ) -> Result<T, Why> {
        let url = super::endpoint(desk, &format!("withdrawal/{endpoint}"));
        desk.reader.operator_action(&url, &self.token, input).await
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Agreement {
    agreement: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Intake {
    agreement: String,
    request_ref: String,
    received_at: String,
    channel: Channel,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Channel {
    Email,
    Letter,
    Phone,
    InPerson,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Outcome {
    Hold,
    Approve,
    Decline,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Billing {
    Unaffected,
    ReviewRequired,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    agreement: String,
    outcome: Outcome,
    evidence_ref: String,
    billing: Billing,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    agreement: String,
    operation: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Stage {
    Received,
    Held,
    Approved,
    Declined,
}

#[derive(Deserialize)]
struct Case {
    agreement: String,
    account: String,
    offer_reference: String,
    #[serde(with = "time::serde::rfc3339")]
    accepted_at: OffsetDateTime,
    request_ref: String,
    #[serde(with = "time::serde::rfc3339")]
    received_at: OffsetDateTime,
    channel: Channel,
    recorded_by: String,
    stage: Stage,
    decision: Option<Decision>,
}

#[derive(Deserialize)]
struct Decision {
    agreement: String,
    outcome: Outcome,
    evidence_ref: String,
    billing: Billing,
    actor: String,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
    operation: String,
}

#[derive(Deserialize)]
struct Review {
    operation: String,
    #[serde(flatten)]
    proposal: Proposal,
    blocks_online: bool,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

fn dossier(value: &str) -> bool {
    value.len() <= 96
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._/:-".contains(&c))
}

impl Case {
    fn valid(&self, agreement: &str) -> bool {
        self.agreement == agreement
            && valid_reference(&self.offer_reference)
            && crate::retail::valid_account(&self.account)
            && dossier(&self.request_ref)
            && match (&self.decision, self.stage) {
                (None, Stage::Received) => true,
                (Some(d), stage) => {
                    d.agreement == agreement
                        && dossier(&d.evidence_ref)
                        && valid_reference(&d.operation)
                        && matches!(
                            (d.outcome, stage),
                            (Outcome::Hold, Stage::Held)
                                | (Outcome::Approve, Stage::Approved)
                                | (Outcome::Decline, Stage::Declined)
                        )
                }
                _ => false,
            }
    }
}

fn hidden(name: &str, value: &str) -> String {
    format!(
        r#"<input type="hidden" name="{name}" value="{}">"#,
        escape(value)
    )
}

fn button(path: &str, fields: &str, label: &str, lang: Lang) -> String {
    format!(
        r#"<form method="post" action="{path}?lang={}">{fields}<button class="btn ghost">{label}</button></form>"#,
        lang.tag()
    )
}

pub(super) fn link(lang: Lang) -> String {
    format!(
        r#"<p><a class="btn ghost" href="{PATH}?lang={}">{}</a></p>"#,
        lang.tag(),
        lang.pick("Widerruf bearbeiten", "Manage a withdrawal")
    )
}

pub(super) fn open_case(agreement: &str, lang: Lang) -> String {
    button(
        "/contracts/withdrawal/lookup",
        &hidden("agreement", agreement),
        lang.pick("Widerruf öffnen", "Open withdrawal"),
        lang,
    )
}

fn problem(lang: Lang, why: Why, agreement: Option<&str>) -> Response {
    let (code, message) = match why {
        Why::Refused | Why::Status(403) => (StatusCode::FORBIDDEN,lang.pick("Öffnen Sie den Widerruf mit einem berechtigten Schreibzugang auf dieser Seite.", "Open the withdrawal on this site with an authorized writer account.")),
        Why::Status(404) => (StatusCode::NOT_FOUND,lang.pick("Unter dieser Referenz ist kein passender Vertrag oder Beleg verfügbar.", "No matching agreement or document is available under this reference.")),
        Why::Status(409) => (StatusCode::CONFLICT,lang.pick("Der Vorgang hat sich geändert oder diese Referenz ist schon vergeben. Öffnen Sie den Fall erneut.", "The case has changed or this reference is already in use. Reopen the case.")),
        Why::Status(422) => (StatusCode::UNPROCESSABLE_ENTITY,lang.pick("Prüfen Sie die Angaben und ob es sich um einen Privatvertrag handelt.", "Check the details and confirm that this is a private agreement.")),
        Why::Status(n @ (400 | 413 | 415)) => (StatusCode::from_u16(n).unwrap_or(StatusCode::BAD_REQUEST),lang.pick("Prüfen Sie Referenzen, Eingabeformat und Empfangszeit.", "Check the references, input format and time of receipt.")),
        _ => (StatusCode::SERVICE_UNAVAILABLE,lang.pick("Der zuständige Dienst ist nicht erreichbar. Prüfen Sie den Beleg oder Fallstatus, bevor Sie erneut bestätigen.", "The service is unavailable. Check the document or case status before confirming again.")),
    };
    let back = agreement
        .filter(|a| valid_reference(a))
        .map_or_else(|| link(lang), |a| open_case(a, lang));
    with_status(
        shell(
            lang,
            views::title(lang),
            &format!(r#"<p role="alert">{message}</p>{back}"#),
        ),
        code,
    )
}

async fn index(Extension(staff): Extension<Staff>) -> Response {
    views::index(staff.lang)
}

#[derive(Deserialize)]
struct AgreementId {
    agreement: String,
}

async fn lookup(
    State(desk): State<Arc<Page>>,
    Extension(staff): Extension<Staff>,
    Form(input): Form<Agreement>,
) -> Response {
    if !valid_reference(&input.agreement) {
        return problem(staff.lang, Why::Status(400), None);
    }
    match staff.provider::<Case>(&desk, "lookup", &json!(input)).await {
        Ok(case) if case.valid(&input.agreement) => views::case(&desk, &case, &staff),
        Err(Why::Status(404)) => {
            let result = desk
                .reader
                .operator_action::<AgreementId>(
                    &super::endpoint(&desk, "lookup"),
                    &staff.token,
                    &json!(input),
                )
                .await;
            match result {
                Ok(id) if id.agreement == input.agreement => {
                    views::intake(staff.lang, &input.agreement)
                }
                Ok(_) => problem(staff.lang, Why::Unreadable, None),
                Err(why) => problem(staff.lang, why, None),
            }
        }
        Err(why) => problem(staff.lang, why, Some(&input.agreement)),
        Ok(_) => problem(staff.lang, Why::Unreadable, Some(&input.agreement)),
    }
}

fn received_time(raw: &str) -> Option<String> {
    // The native datetime-local field is explicitly labelled UTC. API clients may supply an offset.
    let utc = match raw.len() {
        16 => format!("{raw}:00Z"),
        19 => format!("{raw}Z"),
        _ => raw.to_owned(),
    };
    OffsetDateTime::parse(&utc, &Rfc3339)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

async fn intake(
    State(desk): State<Arc<Page>>,
    Extension(staff): Extension<Staff>,
    Form(mut input): Form<Intake>,
) -> Response {
    let lang = staff.lang;
    if !valid_reference(&input.agreement) || !dossier(&input.request_ref) {
        return problem(lang, Why::Status(400), None);
    }
    let Some(at) = received_time(&input.received_at) else {
        return problem(lang, Why::Status(400), Some(&input.agreement));
    };
    input.received_at = at;
    match staff.provider::<Case>(&desk, "intake", &json!(input)).await {
        Ok(case)
            if case.valid(&input.agreement)
                && case.request_ref == input.request_ref
                && case.channel == input.channel
                && OffsetDateTime::parse(&input.received_at, &Rfc3339).ok()
                    == Some(case.received_at) =>
        {
            views::case(&desk, &case, &staff)
        }
        Ok(_) => problem(lang, Why::Unreadable, Some(&input.agreement)),
        Err(why) => problem(lang, why, Some(&input.agreement)),
    }
}

async fn review(
    State(desk): State<Arc<Page>>,
    Extension(staff): Extension<Staff>,
    Form(input): Form<Proposal>,
) -> Response {
    let lang = staff.lang;
    if !valid_reference(&input.agreement) || !dossier(&input.evidence_ref) {
        return problem(lang, Why::Status(400), None);
    }
    match staff
        .provider::<Review>(&desk, "review", &json!(input))
        .await
    {
        Ok(r)
            if r.proposal.agreement == input.agreement
                && r.proposal.outcome == input.outcome
                && r.proposal.billing == input.billing
                && r.proposal.evidence_ref == input.evidence_ref
                && valid_reference(&r.operation)
                && r.blocks_online == (input.outcome == Outcome::Approve) =>
        {
            views::review(&r, &desk.zone, lang)
        }
        Ok(_) => problem(lang, Why::Unreadable, Some(&input.agreement)),
        Err(why) => problem(lang, why, Some(&input.agreement)),
    }
}

async fn confirm(
    State(desk): State<Arc<Page>>,
    Extension(staff): Extension<Staff>,
    Form(input): Form<Confirmation>,
) -> Response {
    let lang = staff.lang;
    if !valid_reference(&input.agreement) || !valid_reference(&input.operation) {
        return problem(lang, Why::Status(400), None);
    }
    match staff
        .provider::<Case>(&desk, "confirm", &json!(input))
        .await
    {
        Ok(case)
            if case.valid(&input.agreement)
                && case
                    .decision
                    .as_ref()
                    .is_some_and(|d| d.operation == input.operation) =>
        {
            views::case(&desk, &case, &staff)
        }
        Ok(_) => problem(lang, Why::Unreadable, Some(&input.agreement)),
        Err(why) => problem(lang, why, Some(&input.agreement)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_utc_picker_and_an_explicit_offset_identify_the_same_receipt_time() {
        let from_input = received_time("2026-10-25T01:30").unwrap();
        let from_message = received_time("2026-10-25T02:30:00+01:00").unwrap();
        assert_eq!(
            OffsetDateTime::parse(&from_input, &Rfc3339),
            OffsetDateTime::parse(&from_message, &Rfc3339)
        );
        assert!(received_time("2026-02-31T12:00").is_none());
        assert!(received_time("tomorrow").is_none());
    }
}
