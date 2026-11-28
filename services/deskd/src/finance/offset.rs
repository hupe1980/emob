//! Move an invoice's available credit after the ledger reviews both balances.
use std::fmt::Write as _;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Form, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::post;
use axum::{Extension, Router};
use emob_core::Currency;
use emob_service::Actor;
use emob_service::ui::{Lang, escape};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::views::{balance_button, hidden, money, scope_fields};
use super::{Finance, Scope, minor_units};
use crate::Page;
use crate::fetch::Why;
use crate::investigation::{shell, valid_reference, with_status};
use crate::retail::valid_account;

pub(super) fn routes() -> Router<Arc<Page>> {
    Router::new()
        .route("/receivables/offsets/review", post(review))
        .route("/receivables/offsets/confirm", post(confirm))
        .layer(DefaultBodyLimit::max(2048))
}

#[derive(Deserialize, Serialize, PartialEq)]
struct Choice {
    #[serde(flatten)]
    scope: Scope,
    source_number: String,
    target_number: String,
}

impl Choice {
    fn valid(&self) -> bool {
        let invoice = |n: &str| {
            (1..=96).contains(&n.len())
                && n.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"*._/-".contains(&c))
        };
        valid_account(&self.scope.account)
            && invoice(&self.source_number)
            && invoice(&self.target_number)
            && self.source_number != self.target_number
    }
}

#[derive(Deserialize)]
struct Draft {
    #[serde(flatten)]
    choice: Choice,
    amount: String,
}

#[derive(Serialize, Deserialize, PartialEq)]
struct Offset {
    #[serde(flatten)]
    choice: Choice,
    amount_minor: i64,
}

#[derive(Serialize, Deserialize)]
struct Confirmation {
    #[serde(flatten)]
    choice: Choice,
    amount_minor: i64,
    review: String,
}

#[derive(Deserialize)]
pub(super) struct Receipt {
    #[serde(flatten)]
    offset: Offset,
    actor: Actor,
    review: String,
    source_before_minor: i64,
    source_after_minor: i64,
    target_before_minor: i64,
    target_after_minor: i64,
}

impl Receipt {
    pub(super) fn belongs_to(&self, scope: &Scope) -> bool {
        let amount = self.offset.amount_minor;
        self.offset.choice.scope == *scope
            && self.offset.choice.valid()
            && matches!(self.actor, Actor::Operator(_))
            && valid_reference(&self.review)
            && amount > 0
            && self.source_before_minor.checked_add(amount) == Some(self.source_after_minor)
            && self.source_after_minor <= 0
            && self.target_before_minor.checked_sub(amount) == Some(self.target_after_minor)
            && self.target_after_minor >= 0
    }

    fn matches(&self, offset: &Offset, actor: &Actor, review: Option<&str>) -> bool {
        &self.actor == actor
            && &self.offset == offset
            && self.belongs_to(&offset.choice.scope)
            && review.is_none_or(|r| r == self.review)
    }

    pub(super) fn history(&self, lang: Lang) -> String {
        let c = &self.offset.choice;
        format!(
            "<strong>{}</strong><br>{} {} · {} {}<br>{} · <code>{}</code>",
            money(self.offset.amount_minor, c.scope.currency, lang),
            lang.pick("Von", "From"),
            escape(&c.source_number),
            lang.pick("An", "To"),
            escape(&c.target_number),
            escape(&self.actor.to_string()),
            escape(&self.review)
        )
    }

    fn page(&self, lang: Lang, posted: bool) -> Response {
        let offset = &self.offset;
        let choice = &offset.choice;
        let scope = &choice.scope;
        let currency = scope.currency;
        let mut body = format!(
            "<p>{} · {}</p><dl class=\"kv\"><dt>{}</dt><dd><strong>{}</strong></dd></dl>",
            escape(&scope.account),
            currency,
            lang.pick("Verrechnungsbetrag", "Amount to apply"),
            money(offset.amount_minor, currency, lang)
        );
        for (label, number, before, after) in [
            (
                lang.pick("Guthaben auf Rechnung", "Credit on invoice"),
                &choice.source_number,
                credit(self.source_before_minor, currency, lang),
                credit(self.source_after_minor, currency, lang),
            ),
            (
                lang.pick("Offener Betrag auf Rechnung", "Amount due on invoice"),
                &choice.target_number,
                money(self.target_before_minor, currency, lang),
                money(self.target_after_minor, currency, lang),
            ),
        ] {
            let _ = write!(
                body,
                "<section><h2>{label} {}</h2><dl class=\"kv\"><dt>{}</dt><dd>{before}</dd><dt>{}</dt><dd>{after}</dd></dl></section>",
                escape(number),
                lang.pick("Vorher", "Before"),
                lang.pick("Nachher", "After")
            );
        }
        if posted {
            let _ = write!(
                body,
                "<dl class=\"kv\"><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd><code>{}</code></dd></dl>",
                lang.pick("Gebucht von", "Confirmed by"),
                escape(&self.actor.to_string()),
                lang.pick("Buchungsreferenz", "Receipt reference"),
                escape(&self.review)
            );
        } else {
            let _ = write!(
                body,
                "<details><summary>{}</summary>{}</details>",
                lang.pick("Rechnungen oder Betrag ändern", "Edit invoices or amount"),
                fields(
                    scope,
                    &choice.source_number,
                    &choice.target_number,
                    &lang.decimal(Decimal::new(
                        offset.amount_minor,
                        currency.minor_unit_digits()
                    )),
                    "",
                    lang
                )
            );
            body.push_str(&confirm_form(
                offset,
                &self.review,
                lang,
                lang.pick("Verrechnung bestätigen", "Confirm credit offset"),
            ));
        }
        body.push_str(&back(scope, lang));
        shell(
            lang,
            if posted {
                lang.pick("Guthaben verrechnet", "Credit applied")
            } else {
                title(lang)
            },
            &body,
        )
    }
}

// Keep the display valid at the smallest signed minor-unit value.
pub(super) fn credit(due: i64, currency: Currency, lang: Lang) -> String {
    let amount = Decimal::new(due, currency.minor_unit_digits()).abs();
    format!("{} {currency}", lang.decimal(amount))
}

fn title(lang: Lang) -> &'static str {
    lang.pick("Verrechnung prüfen", "Review credit offset")
}

fn back(scope: &Scope, lang: Lang) -> String {
    balance_button(
        lang,
        scope,
        0,
        lang.pick("Kontostand öffnen", "Open account balance"),
    )
}

fn confirm_form(offset: &Offset, review: &str, lang: Lang, button: &str) -> String {
    let c = &offset.choice;
    format!(
        r#"<form method="post" action="/receivables/offsets/confirm?lang={}">{}{}{}{}{}<button class="btn">{button}</button></form>"#,
        lang.tag(),
        scope_fields(&c.scope),
        hidden("source_number", &c.source_number),
        hidden("target_number", &c.target_number),
        hidden("amount_minor", &offset.amount_minor.to_string()),
        hidden("review", review)
    )
}

fn fields(
    scope: &Scope,
    source: &str,
    target: &str,
    amount: &str,
    lists: &str,
    lang: Lang,
) -> String {
    let lists = if lists.is_empty() {
        r#"<datalist id="offset-sources"></datalist><datalist id="offset-targets"></datalist>"#
    } else {
        lists
    };
    format!(
        r#"<form method="post" action="/receivables/offsets/review?lang={tag}" autocomplete="off">{scope}{lists}
<p><label>{source_label}<br><input type="text" name="source_number" list="offset-sources" value="{source}" maxlength="96" spellcheck="false" required></label></p>
<p><label>{target_label}<br><input type="text" name="target_number" list="offset-targets" value="{target}" maxlength="96" spellcheck="false" required></label></p>
<p><label>{amount_label} ({currency})<br><input type="text" name="amount" inputmode="decimal" value="{amount}" maxlength="32" required></label></p>
<button class="btn">{review}</button></form>"#,
        tag = lang.tag(),
        scope = scope_fields(scope),
        currency = scope.currency,
        source_label = lang.pick("Rechnung mit Guthaben", "Invoice with available credit"),
        target_label = lang.pick("Offene Zielrechnung", "Invoice to pay"),
        amount_label = lang.pick("Betrag", "Amount"),
        source = escape(source),
        target = escape(target),
        amount = escape(amount),
        review = title(lang)
    )
}

pub(super) fn form<'a>(
    scope: &Scope,
    candidates: impl Iterator<Item = (&'a str, i64)>,
    lang: Lang,
) -> String {
    let mut sources = String::new();
    let mut targets = String::new();
    for (number, due) in candidates {
        if due < 0 {
            let _ = write!(
                sources,
                r#"<option value="{}">{}</option>"#,
                escape(number),
                credit(due, scope.currency, lang)
            );
        } else if due > 0 {
            let _ = write!(
                targets,
                r#"<option value="{}">{}</option>"#,
                escape(number),
                money(due, scope.currency, lang)
            );
        }
    }
    let lists = format!(
        r#"<datalist id="offset-sources">{sources}</datalist><datalist id="offset-targets">{targets}</datalist>"#
    );
    format!(
        r#"<section class="card"><h2>{}</h2><p>{}</p>{}</section>"#,
        lang.pick("Guthaben verrechnen", "Apply credit"),
        lang.pick(
            "Rechnungen auswählen oder eine Rechnungsnummer von einer anderen Kontoseite eingeben.",
            "Choose invoices or enter an invoice number from another account page."
        ),
        fields(scope, "", "", "", &lists, lang)
    )
}

pub(super) fn problem(lang: Lang, why: Why) -> Response {
    if why == Why::Refused {
        let message = lang.pick("Für die Verrechnung sind Finanzzugang, Schreibrecht und ein Formular aus Heute erforderlich.",
            "Use a finance account with write access and submit the form from Heute.");
        return with_status(
            shell(
                lang,
                title(lang),
                &format!(r#"<p role="alert">{message}</p>"#),
            ),
            StatusCode::FORBIDDEN,
        );
    }
    super::views::problem(lang, why)
}

#[derive(Deserialize)]
struct Evidence {
    #[serde(flatten)]
    scope: Scope,
    number: String,
    due_minor: i64,
}

async fn current(desk: &Page, auth: &Finance, scope: &Scope, number: &str) -> Option<i64> {
    let r = auth
        .ask::<Evidence>(
            desk,
            "withdrawal/evidence",
            &json!({"number":number,"account":scope.account,"currency":scope.currency}),
        )
        .await
        .ok()?;
    (r.number == number && r.scope == *scope).then_some(r.due_minor)
}

async fn changed(desk: &Page, auth: &Finance, offset: &Offset) -> Response {
    let choice = &offset.choice;
    let scope = &choice.scope;
    let lang = auth.lang;
    let mut body = format!(
        r#"<p role="alert">{}</p>"#,
        lang.pick(
            "Diese Verrechnung ist nicht mehr möglich. Belege und Betrag erneut prüfen.",
            "This credit offset is no longer available. Review the invoices and amount again."
        )
    );
    let (source, target) = tokio::join!(
        current(desk, auth, scope, &choice.source_number),
        current(desk, auth, scope, &choice.target_number)
    );
    for (label, number, amount) in [
        (
            lang.pick("Verfügbares Guthaben", "Available credit"),
            &choice.source_number,
            source.map(|v| credit(v.min(0), scope.currency, lang)),
        ),
        (
            lang.pick("Offener Betrag", "Amount due"),
            &choice.target_number,
            target.map(|v| money(v.max(0), scope.currency, lang)),
        ),
    ] {
        let _ = write!(
            body,
            "<p>{label} · {} · <strong>{}</strong></p>",
            escape(number),
            amount
                .as_deref()
                .unwrap_or(lang.pick("Nicht verfügbar", "Unavailable"))
        );
    }
    body.push_str(&fields(
        scope,
        &choice.source_number,
        &choice.target_number,
        &lang.decimal(Decimal::new(
            offset.amount_minor,
            scope.currency.minor_unit_digits(),
        )),
        "",
        lang,
    ));
    body.push_str(&back(scope, lang));
    with_status(shell(lang, title(lang), &body), StatusCode::CONFLICT)
}

async fn review(
    State(desk): State<Arc<Page>>,
    Extension(auth): Extension<Finance>,
    Extension(actor): Extension<Actor>,
    Form(draft): Form<Draft>,
) -> Response {
    let choice = draft.choice;
    let Some(amount_minor) =
        minor_units(&draft.amount, choice.scope.currency).filter(|_| choice.valid())
    else {
        let message = auth.lang.pick(
            "Zwei verschiedene Rechnungen und einen positiven Betrag in der Kontowährung eingeben.",
            "Enter two different invoice numbers and a positive amount in the account currency.",
        );
        let body = format!(
            r#"<p role="alert">{message}</p>{}"#,
            fields(
                &choice.scope,
                &choice.source_number,
                &choice.target_number,
                &draft.amount,
                "",
                auth.lang
            )
        );
        return with_status(
            shell(auth.lang, title(auth.lang), &body),
            StatusCode::BAD_REQUEST,
        );
    };
    let offset = Offset {
        choice,
        amount_minor,
    };
    match auth
        .ask::<Receipt>(&desk, "offsets/review", &json!(offset))
        .await
    {
        Ok(r) if r.matches(&offset, &actor, None) => r.page(auth.lang, false),
        Ok(_) => problem(auth.lang, Why::Unreadable),
        Err(Why::Status(409)) => changed(&desk, &auth, &offset).await,
        Err(why) => problem(auth.lang, why),
    }
}

async fn confirm(
    State(desk): State<Arc<Page>>,
    Extension(auth): Extension<Finance>,
    Extension(actor): Extension<Actor>,
    Form(input): Form<Confirmation>,
) -> Response {
    let request = json!(input);
    let offset = Offset {
        choice: input.choice,
        amount_minor: input.amount_minor,
    };
    if !offset.choice.valid() || offset.amount_minor <= 0 || !valid_reference(&input.review) {
        return problem(auth.lang, Why::Status(400));
    }
    match auth
        .ask::<Receipt>(&desk, "offsets/confirm", &request)
        .await
    {
        Ok(r) if r.matches(&offset, &actor, Some(&input.review)) => r.page(auth.lang, true),
        Err(Why::Status(409)) => changed(&desk, &auth, &offset).await,
        Err(why @ (Why::Refused | Why::Status(400 | 404 | 422))) => problem(auth.lang, why),
        _ => {
            let lang = auth.lang;
            let body = format!(r#"<p role="alert">{}</p>{}{}"#,
                lang.pick("Buchungsstatus unbekannt. Dieselbe Bestätigung erneut senden, um den Beleg abzurufen.",
                    "The posting status is unknown. Retry the same confirmation to retrieve the receipt."),
                confirm_form(&offset,&input.review,lang,lang.pick("Bestätigung erneut senden", "Retry confirmation")), back(&offset.choice.scope,lang));
            with_status(
                shell(lang, title(lang), &body),
                StatusCode::SERVICE_UNAVAILABLE,
            )
        }
    }
}
