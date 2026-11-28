//! Staff review and confirm one change to an existing driver agreement.
use super::{
    Arc, Deserialize, Extension, Form, FormRejection, HeaderMap, Lang, OffsetDateTime,
    OperatorAccess, Page, Params, Query, Receipt, Response, State, StatusCode, TimeZone, Why,
    access_error, action_form, endpoint, error_page, escape, invalid, json, language, masked,
    moment, shell, valid_reference, valid_stock, with_status, writer,
};
use core::fmt::Write as _;

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum Action {
    Replace,
    End,
}

impl Action {
    const fn title(self, lang: Lang) -> &'static str {
        match self {
            Self::Replace => lang.pick("Karte ersetzen", "Replace card"),
            Self::End => lang.pick("Vertrag beenden", "End agreement"),
        }
    }
    const fn effect(self, lang: Lang) -> &'static str {
        match self {
            Self::Replace => lang.pick("Die bisherige Karte wird dauerhaft gesperrt. Die Ersatzkarte gilt für denselben Vertrag, Zahler und Preis.", "The old card will be permanently retired. Its replacement uses the same agreement, payer and price."),
            Self::End => lang.pick("Die Karte wird mit der Bestätigung gesperrt. Der Anbieter erfasst diesen Zeitpunkt als Vertragsende für die Abrechnung.", "Confirmation retires the active card and records the effective contract end for billing."),
        }
    }
}

#[derive(Deserialize)]
pub(super) struct Changed {
    operation: String,
    agreement: String,
    action: Action,
    account: String,
    previous_card: String,
    current_card: Option<String>,
    actor: String,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
}

#[derive(Deserialize)]
struct Preview {
    agreement: String,
    action: Action,
    operation: String,
    account: String,
    current_card: String,
    replacement_card: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    agreement: String,
    action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    stock: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Confirm {
    agreement: String,
    operation: String,
}

fn hidden(key: &str, value: &str) -> String {
    format!(
        "<input type=\"hidden\" name=\"{key}\" value=\"{}\">",
        escape(value)
    )
}
fn confirm_form(id: &str, operation: &str, button: &str, lang: Lang) -> String {
    format!(
        "<form method=\"post\" action=\"/contracts/change/confirm?lang={}\">{}{}<button type=\"submit\" class=\"btn\">{button}</button></form>",
        lang.tag(),
        hidden("agreement", id),
        hidden("operation", operation)
    )
}

impl Changed {
    fn matches(&self, agreement: &str) -> bool {
        self.agreement == agreement
            && valid_reference(&self.operation)
            && crate::retail::valid_account(&self.account)
            && masked(&self.previous_card)
            && match self.action {
                Action::Replace => self.current_card.as_deref().is_some_and(masked),
                Action::End => self.current_card.is_none(),
            }
    }
    fn summary(&self, zone: &TimeZone, lang: Lang) -> String {
        let action = match self.action {
            Action::End => lang.pick("Vertrag beendet", "Agreement ended"),
            Action::Replace => lang.pick("Karte ersetzt", "Card replaced"),
        };
        let mut rows = format!(
            "<dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd>",
            lang.pick("Zeitpunkt", "Effective time"),
            moment(self.at, zone, lang),
            lang.pick("Bearbeitet von", "Changed by"),
            escape(&self.actor),
            lang.pick("Gesperrte Karte", "Retired card"),
            escape(&self.previous_card)
        );
        if let Some(card) = &self.current_card {
            let _ = write!(
                rows,
                "<dt>{}</dt><dd>{}</dd>",
                lang.pick("Ersatzkarte", "Replacement card"),
                escape(card)
            );
        }
        format!("<section class=\"card\"><h2>{action}</h2><dl class=\"kv\">{rows}</dl></section>")
    }
}

pub(super) fn section(
    receipt: &Receipt,
    may_write: bool,
    zone: &TimeZone,
    lang: Lang,
) -> Option<String> {
    let mut body = String::new();
    if let Some(change) = &receipt.lifecycle {
        if !change.matches(&receipt.agreement) || change.account != receipt.account {
            return None;
        }
        body = change.summary(zone, lang);
        if change.action == Action::End {
            return Some(body);
        }
    }
    if may_write && !receipt.account.is_empty() {
        let field = hidden("agreement", &receipt.agreement);
        let _ = write!(
            body,
            "<section class=\"card noprint\"><h2>{}</h2><form method=\"post\" action=\"/contracts/change/preview?lang={}\" autocomplete=\"off\">{field}{}<p><label>{}<br><input type=\"text\" name=\"stock\" maxlength=\"32\" required spellcheck=\"false\"></label></p><button type=\"submit\" class=\"btn\">{}</button></form><form method=\"post\" action=\"/contracts/change/preview?lang={}\">{field}{}<button type=\"submit\" class=\"btn\">{}</button></form></section>",
            lang.pick("Vertrag verwalten", "Manage agreement"),
            lang.tag(),
            hidden("action", "replace"),
            lang.pick("Bestandsnummer der Ersatzkarte", "Replacement stock code"),
            lang.pick("Kartentausch prüfen", "Review replacement"),
            lang.tag(),
            hidden("action", "end"),
            lang.pick("Vertragsende prüfen", "Review termination")
        );
    }
    Some(body)
}

pub(super) async fn preview(
    State(desk): State<Arc<Page>>,
    Extension(access): Extension<OperatorAccess>,
    Query(params): Query<Params>,
    headers: HeaderMap,
    data: Result<Form<Request>, FormRejection>,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    let Ok(token) = writer(&headers, access) else {
        return access_error(lang);
    };
    let Form(input) = match data {
        Ok(v) => v,
        Err(e) => return invalid(lang, e.status()),
    };
    if !valid_reference(&input.agreement)
        || match input.action {
            Action::Replace => !input.stock.as_deref().is_some_and(valid_stock),
            Action::End => input.stock.is_some(),
        }
    {
        return invalid(lang, StatusCode::BAD_REQUEST);
    }
    let r = match desk
        .reader
        .operator_action::<Preview>(&endpoint(&desk, "change/preview"), &token, &json!(input))
        .await
    {
        Ok(r) => r,
        Err(e) => return error_page(e, lang, "change"),
    };
    if r.agreement != input.agreement
        || r.action != input.action
        || !valid_reference(&r.operation)
        || !masked(&r.current_card)
        || !crate::retail::valid_account(&r.account)
        || match r.action {
            Action::Replace => !r.replacement_card.as_deref().is_some_and(masked),
            Action::End => r.replacement_card.is_some(),
        }
    {
        return error_page(Why::Unreadable, lang, "change");
    }
    let mut rows = format!(
        "<dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd>",
        crate::retail::account_label(lang),
        escape(&r.account),
        lang.pick("Bisherige Karte", "Current card"),
        escape(&r.current_card),
        lang.pick("Bestätigen bis", "Confirm by"),
        moment(r.expires_at, &desk.zone, lang)
    );
    if let Some(card) = &r.replacement_card {
        let _ = write!(
            rows,
            "<dt>{}</dt><dd>{}</dd>",
            lang.pick("Ersatzkarte", "Replacement card"),
            escape(card)
        );
    }
    shell(
        lang,
        r.action.title(lang),
        &format!(
            "<section class=\"card\"><p>{}</p><dl class=\"kv\">{rows}</dl></section><div class=\"card-controls\">{}{}</div>",
            r.action.effect(lang),
            confirm_form(
                &r.agreement,
                &r.operation,
                lang.pick("Änderung bestätigen", "Confirm change"),
                lang
            ),
            action_form(
                "lookup",
                "agreement",
                &r.agreement,
                lang.pick("Zurück zum Vertrag", "Back to agreement"),
                lang
            )
        ),
    )
}

pub(super) async fn confirm(
    State(desk): State<Arc<Page>>,
    Extension(access): Extension<OperatorAccess>,
    Query(params): Query<Params>,
    headers: HeaderMap,
    data: Result<Form<Confirm>, FormRejection>,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    let Ok(token) = writer(&headers, access) else {
        return access_error(lang);
    };
    let Form(input) = match data {
        Ok(v) => v,
        Err(e) => return invalid(lang, e.status()),
    };
    if !valid_reference(&input.agreement) || !valid_reference(&input.operation) {
        return invalid(lang, StatusCode::BAD_REQUEST);
    }
    let reopen = action_form(
        "lookup",
        "agreement",
        &input.agreement,
        lang.pick("Vertrag öffnen", "Open agreement"),
        lang,
    );
    let payload = json!({"operation":input.operation,"agreement":input.agreement});
    let response = desk
        .reader
        .operator_action::<Changed>(&endpoint(&desk, "change/confirm"), &token, &payload)
        .await;
    match response {
        Ok(change) if change.matches(&input.agreement) && change.operation == input.operation => {
            let summary = change.summary(&desk.zone, lang);
            shell(
                lang,
                lang.pick("Vertragsänderung", "Agreement change"),
                &format!("{summary}{reopen}"),
            )
        }
        Err(Why::Refused) => access_error(lang),
        Err(Why::Status(400 | 404 | 409 | 422)) => {
            let message = lang.pick(
                "Die Änderung ist nicht mehr verfügbar. Prüfen Sie den Vertrag erneut.",
                "This change is unavailable. Reopen the agreement to review its current state.",
            );
            with_status(
                shell(
                    lang,
                    lang.pick("Vertrag prüfen", "Check agreement"),
                    &format!("<p role=\"alert\">{message}</p>{reopen}"),
                ),
                StatusCode::CONFLICT,
            )
        }
        _ => {
            let message = lang.pick("Die Antwort fehlt. Öffnen Sie den Vertrag oder wiederholen Sie dieselbe Bestätigung.","The response is unavailable. Open the agreement or retry the same confirmation.");
            let retry = confirm_form(
                &input.agreement,
                &input.operation,
                lang.pick("Bestätigung wiederholen", "Retry confirmation"),
                lang,
            );
            with_status(
                shell(
                    lang,
                    lang.pick("Bestätigung prüfen", "Check confirmation"),
                    &format!("<p role=\"alert\">{message}</p>{reopen}{retry}"),
                ),
                StatusCode::BAD_GATEWAY,
            )
        }
    }
}
