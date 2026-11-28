//! Named staff review customer access at its owner before changing a grant.
use std::{fmt::Write as _, sync::Arc};

use axum::extract::{DefaultBodyLimit, Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use emob_service::authority::OperatorAccess;
use emob_service::ui::{Lang, escape};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::fetch::Why;
use crate::investigation::{Params, shell, valid_reference, with_status};
use crate::{Page, language, writer};

pub(super) fn routes() -> Router<Arc<Page>> {
    Router::new()
        .route("/customer-access", get(index))
        .route("/customer-access/open", post(open))
        .route("/customer-access/review", post(review))
        .route("/customer-access/confirm", post(confirm))
        .layer(DefaultBodyLimit::max(2048))
}

fn title(lang: Lang) -> &'static str {
    lang.pick("Portalzugriff", "Customer access")
}

fn page(lang: Lang, content: &str) -> Response {
    shell(lang, title(lang), content)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Subject {
    subject: String,
}

#[derive(Deserialize)]
struct Access {
    subject: String,
    revision: u64,
    accounts: Vec<String>,
    agreements: Vec<String>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Account,
    Agreement,
}

impl Kind {
    const fn key(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Agreement => "agreement",
        }
    }
    fn label(self, lang: Lang) -> &'static str {
        match self {
            Self::Account => lang.pick("Rechnungskonto", "Billing account"),
            Self::Agreement => lang.pick("Ladevertrag", "Driver agreement"),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Change {
    subject: String,
    revision: u64,
    kind: Kind,
    reference: String,
    granted: bool,
}

fn hidden(name: &str, value: &str) -> String {
    format!(
        r#"<input type="hidden" name="{name}" value="{}">"#,
        escape(value)
    )
}

async fn index(
    Extension(access): Extension<OperatorAccess>,
    Query(params): Query<Params>,
    headers: HeaderMap,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    if !access.can_write() {
        return with_status(
            page(
                lang,
                lang.pick(
                    "Dafür ist Schreibzugriff erforderlich.",
                    "A writer account is required.",
                ),
            ),
            StatusCode::FORBIDDEN,
        );
    }
    page(
        lang,
        &format!(
            r#"<form class="card" method="post" action="/customer-access/open?lang={}" autocomplete="off"><label>{}<br><input type="text" name="subject" required maxlength="64" spellcheck="false"></label><p><button class="btn">{}</button></p></form>"#,
            lang.tag(),
            lang.pick("Portal-Kennung", "Customer sign-in ID"),
            lang.pick("Zugriff öffnen", "Open access")
        ),
    )
}

impl Access {
    fn render(&self, lang: Lang, saved: bool) -> Response {
        let id = escape(&self.subject);
        let scope = format!(
            "{}{}",
            hidden("subject", &self.subject),
            hidden("revision", &self.revision.to_string())
        );
        let mut out = format!("<section><h2>{id}</h2>");
        if saved {
            let _ = write!(
                out,
                "<p role=\"status\">{}</p>",
                lang.pick("Zugriff gespeichert.", "Access saved.")
            );
        }
        if self.accounts.is_empty() && self.agreements.is_empty() {
            let _ = write!(
                out,
                "<p>{}</p>",
                lang.pick("Keine Berechtigungen vergeben.", "No access assigned.")
            );
        }
        for (kind, ids) in [
            (Kind::Account, &self.accounts),
            (Kind::Agreement, &self.agreements),
        ] {
            for reference in ids {
                let _ = write!(
                    out,
                    r#"<form method="post" action="/customer-access/review?lang={}" class="card">{scope}{}{}{}<strong>{}</strong><p class="mono">{}</p><button class="btn ghost">{}</button></form>"#,
                    lang.tag(),
                    hidden("kind", kind.key()),
                    hidden("reference", reference),
                    hidden("granted", "false"),
                    kind.label(lang),
                    escape(reference),
                    lang.pick("Entzug prüfen", "Review removal")
                );
            }
        }
        let _ = write!(
            out,
            r#"</section><section class="card"><h2>{}</h2><form method="post" action="/customer-access/review?lang={}">{scope}{}<p><label>{}<br><select name="kind"><option value="agreement">{}</option><option value="account">{}</option></select></label></p><p><label>{}<br><input name="reference" type="text" maxlength="64" required autocomplete="off" spellcheck="false"></label></p><button class="btn">{}</button></form></section>"#,
            lang.pick("Berechtigung vergeben", "Assign access"),
            lang.tag(),
            hidden("granted", "true"),
            lang.pick("Berechtigung für", "Access to"),
            Kind::Agreement.label(lang),
            Kind::Account.label(lang),
            lang.pick(
                "Vertrags- oder Kontoreferenz",
                "Agreement or account reference"
            ),
            lang.pick("Zuweisung prüfen", "Review assignment")
        );
        page(lang, &out)
    }
}

fn problem(lang: Lang, why: Why) -> Response {
    let (status, text) = match why {
        Why::Refused => (
            StatusCode::FORBIDDEN,
            lang.pick("Zugriff verweigert.", "Access denied."),
        ),
        Why::Status(404) => (
            StatusCode::NOT_FOUND,
            lang.pick(
                "Kennung oder Referenz ist unbekannt.",
                "Unknown sign-in ID or reference.",
            ),
        ),
        Why::Status(409) => (
            StatusCode::CONFLICT,
            lang.pick(
                "Der Zugriff hat sich geändert. Öffnen Sie ihn erneut.",
                "Access has changed. Open it again to review.",
            ),
        ),
        Why::Status(400) => (
            StatusCode::BAD_REQUEST,
            lang.pick(
                "Kennung oder Referenz prüfen.",
                "Check the sign-in ID and reference.",
            ),
        ),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            lang.pick(
                "Der Mobilitätsanbieter ist nicht erreichbar. Zugriff erneut öffnen.",
                "The provider is unavailable. Reopen access to check the outcome.",
            ),
        ),
    };
    let body = format!(
        r#"<p role="alert">{text}</p><a class="btn ghost" href="/customer-access?lang={}">{}</a>"#,
        lang.tag(),
        lang.pick("Kundenzugriff öffnen", "Open customer access")
    );
    with_status(page(lang, &body), status)
}

async fn open(
    State(desk): State<Arc<Page>>,
    Extension(access): Extension<OperatorAccess>,
    Query(params): Query<Params>,
    headers: HeaderMap,
    Form(input): Form<Subject>,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    let token = match writer(&headers, access) {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    let url = format!(
        "{}/v1/empd/customer/access",
        desk.empd.trim_end_matches('/')
    );
    match desk
        .reader
        .operator_action::<Access>(&url, &token, &json!({"subject":input.subject}))
        .await
    {
        Ok(a) if a.subject == input.subject => a.render(lang, false),
        Ok(_) => problem(lang, Why::Unreadable),
        Err(why) => problem(lang, why),
    }
}

async fn review(
    Extension(access): Extension<OperatorAccess>,
    Query(params): Query<Params>,
    headers: HeaderMap,
    Form(change): Form<Change>,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    if let Err(e) = writer(&headers, access) {
        return e.into_response();
    }
    let valid = match change.kind {
        Kind::Account => crate::retail::valid_account(&change.reference),
        Kind::Agreement => valid_reference(&change.reference),
    };
    if !valid || change.subject.is_empty() || change.subject.len() > 64 {
        return problem(lang, Why::Status(400));
    }
    let action = if change.granted {
        lang.pick("Zugriff gewähren", "Grant access")
    } else {
        lang.pick("Zugriff entziehen", "Remove access")
    };
    page(
        lang,
        &format!(
            r#"<section class="card"><h2>{action}</h2><dl class="kv"><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd></dl><p>{}</p><form method="post" action="/customer-access/confirm?lang={}">{}{}{}{}{}<button class="btn">{}</button></form></section>"#,
            lang.pick("Portal-Kennung", "Customer sign-in ID"),
            escape(&change.subject),
            change.kind.label(lang),
            escape(&change.reference),
            match change.kind {
                Kind::Account => lang.pick(
                    "Gilt für alle Rechnungen und Gutschriften dieses Kontos.",
                    "Applies to all invoices and credits for this account."
                ),
                Kind::Agreement => lang.pick(
                    "Gilt für den Vertrag und seine Ladebelege.",
                    "Applies to this agreement and its charging receipts."
                ),
            },
            lang.tag(),
            hidden("subject", &change.subject),
            hidden("revision", &change.revision.to_string()),
            hidden("kind", change.kind.key()),
            hidden("reference", &change.reference),
            hidden("granted", if change.granted { "true" } else { "false" }),
            lang.pick("Bestätigen", "Confirm change")
        ),
    )
}

async fn confirm(
    State(desk): State<Arc<Page>>,
    Extension(access): Extension<OperatorAccess>,
    Query(params): Query<Params>,
    headers: HeaderMap,
    Form(change): Form<Change>,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    let token = match writer(&headers, access) {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    let url = format!(
        "{}/v1/empd/customer/grants",
        desk.empd.trim_end_matches('/')
    );
    match desk
        .reader
        .operator_action::<Access>(&url, &token, &json!(change))
        .await
    {
        Ok(a) if a.subject == change.subject => a.render(lang, true),
        Ok(_) => problem(lang, Why::Unreadable),
        Err(why) => problem(lang, why),
    }
}
