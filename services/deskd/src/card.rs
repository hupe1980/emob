//! Named operator card decisions. The provider owns both the preview and commit.
use core::fmt::Write as _;
use std::sync::Arc;

use axum::Extension;
use axum::extract::rejection::FormRejection;
use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use emob_core::TimeZone;
use emob_service::authority::OperatorAccess;
use emob_service::ui::{Lang, escape};
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;

use crate::fetch::Why;
use crate::investigation::{Params, moment, post_button, shell, valid_reference, with_status};
use crate::{Page, language, writer};

#[derive(Deserialize)]
pub(super) struct Cards {
    pub items: Vec<Card>,
    pub next: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct Card {
    pub reference: String,
    pub visual: String,
    pub blocked: bool,
    history: Vec<Applied>,
}

#[derive(Deserialize)]
pub(super) struct Request {
    reference: String,
    blocked: bool,
    reason: String,
}

#[derive(Deserialize)]
pub(super) struct Confirmation {
    operation: String,
    reference: String,
}

#[derive(Deserialize)]
struct Review {
    operation: String,
    reference: String,
    visual: String,
    blocked: bool,
    reason: String,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    effect: Effect,
    offline_cache: Cache,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Effect {
    RefuseOnline,
    EvaluateContract,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Cache {
    Unchanged,
}

#[derive(Deserialize)]
pub(super) struct Applied {
    operation: String,
    reference: String,
    visual: String,
    blocked: bool,
    reason: String,
    actor: String,
    #[serde(with = "time::serde::rfc3339")]
    reviewed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
}

pub(super) const fn status(blocked: bool, lang: Lang) -> &'static str {
    if blocked {
        lang.pick("Karte gesperrt", "Card blocked")
    } else {
        lang.pick("Karte nicht gesperrt", "Card unblocked")
    }
}

fn reasons(block: bool) -> &'static [&'static str] {
    if block {
        &["lost", "stolen", "suspected_misuse", "correction"]
    } else {
        &["recovered", "cleared", "correction"]
    }
}

fn reason(code: &str, lang: Lang) -> &'static str {
    match code {
        "lost" => lang.pick("Verlust gemeldet", "Reported lost"),
        "stolen" => lang.pick("Diebstahl gemeldet", "Reported stolen"),
        "suspected_misuse" => lang.pick("Verdacht auf Missbrauch", "Suspected misuse"),
        "recovered" => lang.pick("Karte wiedergefunden", "Card recovered"),
        "cleared" => lang.pick("Verdacht ausgeräumt", "Concern resolved"),
        "correction" => lang.pick("Korrektur", "Correction"),
        _ => lang.pick("Unbekannter Grund", "Unknown reason"),
    }
}

fn open_card(reference: &str, lang: Lang, label: &str) -> String {
    post_button("reference", reference, None, label, lang)
}

fn preview_form(card: &Card, lang: Lang) -> String {
    let block = !card.blocked;
    let mut options = String::new();
    for code in reasons(block) {
        let _ = write!(
            options,
            "<option value=\"{code}\">{}</option>",
            reason(code, lang)
        );
    }
    let (heading, review) = if block {
        (
            lang.pick("Karte sperren", "Block card"),
            lang.pick("Sperrung prüfen", "Review block"),
        )
    } else {
        (
            lang.pick("Karte entsperren", "Unblock card"),
            lang.pick("Entsperrung prüfen", "Review unblock"),
        )
    };
    format!(
        "<section class=\"card noprint\"><h2>{heading}</h2><form method=\"post\" action=\"/cards/preview?lang={tag}\"><input type=\"hidden\" name=\"reference\" value=\"{reference}\"><input type=\"hidden\" name=\"blocked\" value=\"{block}\"><p><label>{reason}<select name=\"reason\" required><option value=\"\">{choose}</option>{options}</select></label></p><button class=\"btn\" type=\"submit\">{review}</button></form></section>",
        tag = lang.tag(),
        reference = escape(&card.reference),
        reason = lang.pick("Grund", "Reason"),
        choose = lang.pick("Grund auswählen", "Choose a reason"),
    )
}

fn action_summary(action: &Applied, zone: &TimeZone, lang: Lang) -> String {
    format!(
        "<strong>{state}</strong> · {reason}<br>{actor}<br>{reviewed} {review_time} · {applied} {time}<br><span class=\"muted\">{ref_label} <code>{id}</code></span>",
        state = status(action.blocked, lang),
        reason = reason(&action.reason, lang),
        actor = escape(&action.actor),
        reviewed = lang.pick("Geprüft", "Reviewed"),
        review_time = moment(action.reviewed_at, zone, lang),
        applied = lang.pick("Bestätigt", "Applied"),
        time = moment(action.at, zone, lang),
        id = escape(&action.operation),
        ref_label = lang.pick("Aktionsreferenz", "Action reference")
    )
}

pub(super) fn view(
    card: &Card,
    zone: &TimeZone,
    lang: Lang,
    can_write: bool,
    result: Option<&Applied>,
) -> Response {
    let mut body = String::new();
    if let Some(action) = result {
        let _ = write!(
            body,
            "<section class=\"card\" role=\"status\"><h2>{}</h2><p>{}</p></section>",
            lang.pick("Änderung bestätigt", "Change confirmed"),
            action_summary(action, zone, lang)
        );
    }
    let _ = write!(
        body,
        "<section class=\"card\"><h2>{state}</h2><p>{label} <strong>{visual}</strong></p></section>",
        state = status(card.blocked, lang),
        label = lang.pick("emob-Suchnummer", "emob lookup number"),
        visual = escape(&card.visual)
    );
    if can_write {
        body.push_str(&preview_form(card, lang));
    } else {
        let _ = write!(body, "<p>{}</p>", crate::read_only(lang));
    }
    let _ = write!(
        body,
        "<section class=\"card\" id=\"card-history\"><h2>{}</h2>",
        lang.pick("Letzte Kartenänderungen", "Recent card changes")
    );
    if card.history.is_empty() {
        let _ = write!(
            body,
            "<p>{}</p>",
            lang.pick(
                "Noch keine Änderung durch den Betrieb.",
                "No operator changes recorded."
            )
        );
    } else {
        body.push_str("<ol class=\"timeline\">");
        for change in card
            .history
            .iter()
            .filter(|entry| entry.reference == card.reference)
            .take(10)
        {
            let _ = write!(body, "<li>{}</li>", action_summary(change, zone, lang));
        }
        body.push_str("</ol>");
    }
    body.push_str("</section>");
    shell(lang, lang.pick("Ladekarte", "Charging card"), &body)
}

fn confirm_form(operation: &str, reference: &str, lang: Lang, button: &str, class: &str) -> String {
    format!(
        "<form class=\"noprint\" method=\"post\" action=\"/cards/confirm?lang={}\"><input type=\"hidden\" name=\"operation\" value=\"{}\"><input type=\"hidden\" name=\"reference\" value=\"{}\"><button type=\"submit\" class=\"btn {class}\">{button}</button></form>",
        lang.tag(),
        escape(operation),
        escape(reference)
    )
}

fn review_page(review: &Review, lang: Lang, zone: &TimeZone) -> Response {
    let (title, label, class) = if review.blocked {
        (
            lang.pick("Sperrung prüfen", "Review block"),
            lang.pick("Sperrung bestätigen", "Confirm block"),
            "danger",
        )
    } else {
        (
            lang.pick("Entsperrung prüfen", "Review unblock"),
            lang.pick("Entsperrung bestätigen", "Confirm unblock"),
            "",
        )
    };
    let effect = match review.effect {
        Effect::RefuseOnline => lang.pick("Bei der nächsten Online-Autorisierung lehnt der Anbieter diese Karte ab.", "The provider will decline this card at its next online authorization."),
        Effect::EvaluateContract => lang.pick("Der Anbieter prüft bei der nächsten Online-Autorisierung wieder die regulären Vertrags- und Zahlungsbedingungen.", "At the next online authorization the provider will apply its usual contract and credit checks."),
    };
    let Cache::Unchanged = review.offline_cache;
    let body = format!(
        "<section class=\"card\"><h2>{}</h2><dl class=\"kv\"><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd></dl><p>{effect}</p><p>{}</p></section><div class=\"card-controls\">{}{}</div>",
        lang.pick("Vorschau", "Preview"),
        lang.pick("Ladekarte", "Charging card"),
        escape(&review.visual),
        lang.pick("Grund", "Reason"),
        reason(&review.reason, lang),
        lang.pick("Bestätigen bis", "Confirm by"),
        moment(review.expires_at, zone, lang),
        lang.pick(
            "Eine Station ohne Verbindung kann ihre gespeicherte Freigabe weiter verwenden. Laufende Ladevorgänge laufen weiter.",
            "An offline station can continue using its cached authorization. Charging sessions already in progress continue."
        ),
        confirm_form(&review.operation, &review.reference, lang, label, class),
        open_card(
            &review.reference,
            lang,
            lang.pick("Zurück zur Karte", "Back to card")
        )
    );
    shell(lang, title, &body)
}

fn problem(code: StatusCode, message: &str, lang: Lang, actions: &str) -> Response {
    with_status(
        shell(
            lang,
            lang.pick("Kartenänderung", "Card change"),
            &format!("<p role=\"alert\">{message}</p>{actions}"),
        ),
        code,
    )
}

fn forbidden(lang: Lang) -> Response {
    problem(
        StatusCode::FORBIDDEN,
        lang.pick(
            "Für diese Änderung ist ein Schreibzugang auf derselben Seite erforderlich.",
            "Open the card on this site with an account that can make changes.",
        ),
        lang,
        "",
    )
}

fn invalid(lang: Lang) -> Response {
    problem(
        StatusCode::BAD_REQUEST,
        lang.pick(
            "Ungültige Kartenauswahl. Karte erneut öffnen und einen Grund auswählen.",
            "Invalid card selection. Reopen the card and choose a reason.",
        ),
        lang,
        "",
    )
}

fn owner_error(error: Why, reference: &str, operation: Option<&str>, lang: Lang) -> Response {
    let (code, message) = match error {
        Why::Refused => (StatusCode::FORBIDDEN, lang.pick(
            "Der Anbieter hat die Änderung für diesen Zugang abgelehnt.",
            "The provider denied this account permission to change the card.",
        )),
        Why::Status(409) => (StatusCode::CONFLICT, lang.pick(
            "Die Vorschau ist abgelaufen, wurde ersetzt oder der Zielstatus ist bereits gesetzt. Prüfen Sie den aktuellen Verlauf.",
            "The preview expired, was replaced, or the card is already in the requested state. Check the current history.",
        )),
        Why::Status(404) => (StatusCode::NOT_FOUND, lang.pick(
            "Diese Karte ist beim Anbieter nicht mehr vorhanden.",
            "The provider no longer has this card.",
        )),
        Why::Status(400 | 422) => return invalid(lang),
        _ => (StatusCode::BAD_GATEWAY, lang.pick(
            "Der Anbieter kann die Vorschau gerade nicht liefern. Die Karte erneut öffnen und später versuchen.",
            "The provider could not supply a preview. Reopen the card and try again later.",
        )),
    };
    let mut actions = String::new();
    if let Some(id) = operation {
        let _ = write!(
            actions,
            "<p>{} <code>{}</code></p>",
            lang.pick("Aktionsreferenz", "Action reference"),
            escape(id)
        );
    }
    actions.push_str(&open_card(
        reference,
        lang,
        lang.pick("Karte öffnen", "Open card"),
    ));
    problem(code, message, lang, &actions)
}

pub(super) async fn preview(
    State(desk): State<Arc<Page>>,
    Extension(access): Extension<OperatorAccess>,
    Query(params): Query<Params>,
    headers: HeaderMap,
    form: Result<Form<Request>, FormRejection>,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    let Ok(token) = writer(&headers, access) else {
        return forbidden(lang);
    };
    let Form(input) = match form {
        Ok(form) => form,
        Err(error) => return with_status(invalid(lang), error.status()),
    };
    if !valid_reference(&input.reference)
        || !reasons(input.blocked).contains(&input.reason.as_str())
    {
        return invalid(lang);
    }
    let url = format!("{}/v1/empd/cards/preview", desk.empd.trim_end_matches('/'));
    let payload =
        json!({"reference":input.reference,"blocked":input.blocked,"reason":input.reason});
    match desk
        .reader
        .operator_action::<Review>(&url, &token, &payload)
        .await
    {
        Ok(review)
            if review.reference == input.reference
                && review.reason == input.reason
                && review.blocked == input.blocked
                && valid_reference(&review.operation)
                && matches!(
                    (&review.effect, review.blocked),
                    (Effect::RefuseOnline, true) | (Effect::EvaluateContract, false)
                ) =>
        {
            review_page(&review, lang, &desk.zone)
        }
        Ok(_) => owner_error(Why::Unreadable, &input.reference, None, lang),
        Err(why) => owner_error(why, &input.reference, None, lang),
    }
}

fn unconfirmed(form: &Confirmation, lang: Lang) -> Response {
    let actions = open_card(
        &form.reference,
        lang,
        lang.pick("Kartenverlauf prüfen", "Check card history"),
    ) + &confirm_form(
        &form.operation,
        &form.reference,
        lang,
        lang.pick(
            "Dieselbe Bestätigung wiederholen",
            "Retry this confirmation",
        ),
        "ghost",
    );
    let message = format!(
        "{} <br>{} <code>{}</code>",
        lang.pick(
            "Die Antwort des Anbieters fehlt. Die Änderung kann bereits wirksam sein. Prüfen Sie den Kartenverlauf.",
            "The provider’s response is unavailable. The change may have taken effect. Check the card history."
        ),
        lang.pick("Aktionsreferenz", "Action reference"),
        escape(&form.operation),
    );
    problem(StatusCode::BAD_GATEWAY, &message, lang, &actions)
}

pub(super) async fn confirm(
    State(desk): State<Arc<Page>>,
    Extension(access): Extension<OperatorAccess>,
    Query(params): Query<Params>,
    headers: HeaderMap,
    form: Result<Form<Confirmation>, FormRejection>,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    let Ok(token) = writer(&headers, access) else {
        return forbidden(lang);
    };
    let Form(form) = match form {
        Ok(form) => form,
        Err(error) => return with_status(invalid(lang), error.status()),
    };
    if !(valid_reference(&form.operation) && valid_reference(&form.reference)) {
        return invalid(lang);
    }
    let url = format!("{}/v1/empd/cards/confirm", desk.empd.trim_end_matches('/'));
    let result = match desk
        .reader
        .operator_action::<Applied>(
            &url,
            &token,
            &json!({"operation":form.operation, "reference":form.reference}),
        )
        .await
    {
        Ok(action) if action.operation == form.operation && action.reference == form.reference => {
            action
        }
        Err(why @ (Why::Refused | Why::Status(400 | 404 | 409 | 422))) => {
            return owner_error(why, &form.reference, Some(&form.operation), lang);
        }
        _ => return unconfirmed(&form, lang),
    };
    let url = format!("{}/v1/empd/cards/lookup", desk.empd.trim_end_matches('/'));
    if let Ok(cards) = desk
        .reader
        .lookup::<Cards>(&url, &json!({"kind":"reference","value":result.reference}))
        .await
        && let [card] = cards.items.as_slice()
        && card.reference == result.reference
    {
        return view(card, &desk.zone, lang, access.can_write(), Some(&result));
    }
    // The commit was acknowledged even if the following read failed.
    let body = format!(
        "<p role=\"status\"><strong>{}</strong> · {}</p><p>{}</p><p>{}</p>{}",
        lang.pick("Änderung bestätigt", "Change confirmed"),
        escape(&result.visual),
        action_summary(&result, &desk.zone, lang),
        lang.pick(
            "Der aktuelle Verlauf ist vorübergehend nicht verfügbar.",
            "The current history is temporarily unavailable."
        ),
        open_card(
            &result.reference,
            lang,
            lang.pick("Kartenverlauf öffnen", "Open card history")
        )
    );
    shell(lang, lang.pick("Ladekarte", "Charging card"), &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn markup(response: Response) -> String {
        let body = axum::body::to_bytes(response.into_body(), 150_000)
            .await
            .expect("initial card page stays within the desk budget");
        String::from_utf8(body.to_vec()).unwrap()
    }

    // heute-acts/SR-034: a private receipt remains legible in both languages.
    #[tokio::test]
    async fn bilingual_reviews_and_history_are_bounded_and_script_free() {
        let reference = "d".repeat(64);
        let operation = "a".repeat(64);
        let zone = TimeZone::new("Europe/Berlin").unwrap();
        let history = (0..12)
            .map(|n| json!({
                "reference":reference, "operation":format!("{n:064x}"),
                "visual":"EM-0123456789AB", "blocked":true, "reason":"lost",
                "actor":"operator:<crew>", "reviewed_at":"2026-11-25T08:00:00Z", "at":"2026-11-25T08:01:00Z"
            }))
            .collect::<Vec<_>>();
        let card: Card = serde_json::from_value(json!({
            "reference":reference, "visual":"EM-0123456789AB", "blocked":true, "history":history
        }))
        .unwrap();
        let review: Review = serde_json::from_value(json!({
            "reference":reference, "operation":operation, "visual":"EM-0123456789AB",
            "blocked":false, "reason":"recovered", "expires_at":"2026-11-25T08:05:00Z",
            "effect":"evaluate_contract", "offline_cache":"unchanged"
        }))
        .unwrap();
        for (lang, confirm, back, history) in [
            (
                Lang::De,
                "Entsperrung bestätigen",
                "Zurück zur Karte",
                "Letzte Kartenänderungen",
            ),
            (
                Lang::En,
                "Confirm unblock",
                "Back to card",
                "Recent card changes",
            ),
        ] {
            let review = markup(review_page(&review, lang, &zone)).await;
            assert!(review.contains(confirm) && review.contains(back));
            assert!(review.contains("2026-11-25T08:05:00Z"));
            let card_page = markup(view(&card, &zone, lang, false, None)).await;
            assert!(card_page.contains(history) && !card_page.contains("/cards/preview"));
            assert_eq!(card_page.matches("operator:&lt;crew&gt;").count(), 10);
            assert!(!card_page.contains("<crew>"));
            for page in [&review, &card_page] {
                assert!(!page.contains("<script"));
                for attribute in ["href=\"", "action=\""] {
                    for target in page
                        .split(attribute)
                        .skip(1)
                        .map(|s| s.split('"').next().unwrap())
                    {
                        assert!(!target.contains(&reference) && !target.contains(&operation));
                    }
                }
            }
        }
    }
}
