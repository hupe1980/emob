use std::{collections::BTreeSet, fmt::Write as _};

use axum::{http::StatusCode, response::Response};
use emob_core::Currency;
use emob_service::ui::{Lang, escape};
use rust_decimal::Decimal;
use serde::Deserialize;

use super::{Allocation, Draft, Preview, Scope};
use crate::fetch::Why;
use crate::investigation::{shell, with_status};

pub(super) fn money(value: i64, currency: Currency, lang: Lang) -> String {
    let decimal = Decimal::new(value, currency.minor_unit_digits());
    format!("{} {currency}", lang.decimal(decimal))
}

pub(super) fn hidden(name: &str, value: &str) -> String {
    format!(
        r#"<input type="hidden" name="{name}" value="{}">"#,
        escape(value)
    )
}

pub(super) fn scope_fields(scope: &Scope) -> String {
    hidden("account", &scope.account) + &hidden("currency", scope.currency.as_str())
}

fn back(lang: Lang) -> String {
    format!(
        r#"<p><a class="btn ghost" href="/receivables?lang={}">{}</a></p>"#,
        lang.tag(),
        lang.pick("Zur Forderungsübersicht", "Return to receivables")
    )
}

pub(super) fn balance_button(lang: Lang, scope: &Scope, page: u32, label: &str) -> String {
    format!(
        r#"<form method="post" action="/receivables/balance?lang={}">{}<input type="hidden" name="page" value="{page}"><button class="btn ghost">{label}</button></form>"#,
        lang.tag(),
        scope_fields(scope)
    )
}

fn title(lang: Lang) -> &'static str {
    lang.pick("Forderungen", "Receivables")
}

pub(super) fn index(lang: Lang) -> Response {
    let body = format!(r#"<section class="card"><h2>{balance}</h2>
<form method="post" action="/receivables/balance?lang={tag}" autocomplete="off">
<p><label>{account}<br><input type="text" name="account" maxlength="32" required></label></p>
<p><label>{currency}<br><input type="text" name="currency" value="EUR" minlength="3" maxlength="3" required></label></p>
<button class="btn">{open}</button></form></section>
<section class="card"><h2>{import}</h2><form method="post" enctype="multipart/form-data" action="/receivables/upload?lang={tag}">
<p><label for="file">{file}</label><br><input id="file" type="file" name="file" required accept=".json,application/json" aria-describedby="file-help"><br><small id="file-help">{file_help}</small></p>
<button class="btn">{upload}</button></form>
<details><summary>{paste}</summary><form method="post" action="/receivables/import?lang={tag}" autocomplete="off">
<p><label for="statement">{statement}</label><br><textarea id="statement" name="statement" spellcheck="false" required aria-describedby="statement-help"></textarea><br><small id="statement-help">{help}</small></p>
<button class="btn">{import}</button></form></details></section>"#,
        tag=lang.tag(), balance=lang.pick("Kontostand", "Account balance"), account=lang.pick("Rechnungskonto", "Billing account"),
        currency=lang.pick("Währung", "Currency"), open=lang.pick("Konto öffnen", "Open account"),
        file=lang.pick("Auszugsdatei (JSON)", "Statement file (JSON)"), file_help=lang.pick("Version 1 · bis zu 100 Einträge und 64 KiB pro Datei.", "Version 1 · up to 100 entries and 64 KiB per file."),
        upload=lang.pick("Datei importieren", "Import file"), paste=lang.pick("Inhalt einfügen", "Paste statement contents"),
        import=lang.pick("Kontoauszug importieren", "Import statement"), statement=lang.pick("Auszugsdatei (JSON)", "Statement file contents (JSON)"),
        help=lang.pick("Inhalt einer lokalen Auszugsdatei in Version 1 einfügen. Beträge stehen in der kleinsten Währungseinheit.",
            "Paste the contents of a local version 1 statement file. Amounts use the currency’s minor unit."));
    shell(lang, title(lang), &body)
}

pub(super) fn problem(lang: Lang, why: Why) -> Response {
    let (status, message) = match why {
        Why::Refused => (StatusCode::FORBIDDEN, lang.pick("Ein Finanzzugang und die Übermittlung direkt aus Heute sind erforderlich.", "Use a finance account and submit forms from Heute.")),
        Why::Status(413) => (StatusCode::PAYLOAD_TOO_LARGE, lang.pick("Die Auszugsdatei darf höchstens 64 KiB groß sein.", "The statement file must be 64 KiB or smaller.")),
        Why::Status(400 | 415 | 422) => (StatusCode::BAD_REQUEST, lang.pick("Bitte Kontokennung, Währung, Referenzen, Auszugsformat und Betrag prüfen.", "Check the account, currency, references, statement format and amount.")),
        Why::Status(404) => (StatusCode::NOT_FOUND, lang.pick("Konto, Zahlung oder Beleg nicht gefunden. Prüfen Sie die Referenz.", "Account, payment or document not found. Check the reference.")),
        Why::Status(409) => (StatusCode::CONFLICT, lang.pick("Diese Zuordnung ist nicht mehr möglich. Kontostand erneut prüfen.", "This allocation is no longer available. Review the current account balance.")),
        _ => (StatusCode::SERVICE_UNAVAILABLE, lang.pick("Das Nebenbuch ist nicht erreichbar. Nach einem Sendeversuch den Kontostand prüfen.", "The ledger is unavailable. If you submitted an allocation, check the account balance when it returns.")),
    };
    with_status(
        shell(
            lang,
            title(lang),
            &format!(r#"<p role="alert">{message}</p>{}"#, back(lang)),
        ),
        status,
    )
}

#[derive(Deserialize)]
pub(super) struct Balance {
    #[serde(flatten)]
    scope: Scope,
    issuer: String,
    can_allocate: bool,
    can_offset: bool,
    net_due_minor: i64,
    credit_minor: i64,
    unapplied_minor: i64,
    documents: Vec<Document>,
    payments: Vec<Payment>,
    allocations: Vec<Applied>,
    offsets: Vec<super::offset::Receipt>,
    held: Vec<Held>,
    next_page: Option<u32>,
}

#[derive(Deserialize)]
struct Document {
    number: String,
    kind: DocumentKind,
    predecessor: Option<String>,
    amount_minor: i64,
    open_minor: i64,
    due_minor: i64,
    incoming_minor: i64,
    outgoing_minor: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DocumentKind {
    Invoice,
    Credit,
    LineCredit,
    Replacement,
}

#[derive(Deserialize)]
struct Payment {
    source: String,
    reference: String,
    amount_minor: i64,
    unapplied_minor: i64,
}

#[derive(Deserialize)]
struct Applied {
    source: String,
    reference: String,
    number: String,
    amount_minor: i64,
    actor: String,
}

#[derive(Deserialize)]
struct Held {
    amount_minor: i64,
    source: String,
    reference: Option<String>,
    reason: String,
}

fn reason(code: &str, lang: Lang) -> &'static str {
    match code {
        "missing_reference" => lang.pick("Buchungsreferenz fehlt", "Missing entry reference"),
        "not_booked" => lang.pick("Noch nicht gebucht", "Entry is not booked"),
        "not_credit" => lang.pick("Kein Zahlungseingang", "Entry is not an incoming payment"),
        "reversal" => lang.pick("Storno prüfen", "Review the reversal"),
        "invalid_amount" => lang.pick("Ungültiger Betrag", "Invalid amount"),
        "reference_conflict" => lang.pick(
            "Abweichende Daten zur selben Referenz",
            "Existing reference has different details",
        ),
        "invalid_reference" => lang.pick("Ungültige Buchungsreferenz", "Invalid entry reference"),
        _ => lang.pick("Eintrag prüfen", "Review the entry"),
    }
}

fn row_section(title: &str, rows: impl Iterator<Item = String>, lang: Lang) -> String {
    let mut out = format!("<section><h2>{title}</h2>");
    let mut rows = rows.peekable();
    if rows.peek().is_some() {
        out.push_str(r#"<ul class="rows">"#);
        for row in rows {
            let _ = write!(out, r#"<li><p class="what">{row}</p></li>"#);
        }
        out.push_str("</ul>");
    } else {
        let _ = write!(
            out,
            "<p>{}</p>",
            lang.pick(
                "Keine Einträge auf dieser Seite.",
                "No entries on this page."
            )
        );
    }
    out.push_str("</section>");
    out
}

impl Document {
    fn row(&self, currency: Currency, lang: Lang) -> String {
        let price = |amount| money(amount, currency, lang);
        let kind = match self.kind {
            DocumentKind::Credit => lang.pick("Gutschrift", "Credit"),
            DocumentKind::LineCredit => lang.pick("Positionsgutschrift", "Line credit"),
            DocumentKind::Replacement => lang.pick("Ersatzrechnung", "Replacement invoice"),
            DocumentKind::Invoice => lang.pick("Rechnung", "Invoice"),
        };
        let mut line = format!(
            "<strong>{kind} {}</strong><br>{} · {} {}",
            escape(&self.number),
            price(self.amount_minor),
            lang.pick("Offen", "Open"),
            price(self.open_minor)
        );
        if self.due_minor < 0 {
            let _ = write!(
                line,
                "<br>{} {}",
                lang.pick("Verfügbares Guthaben", "Available credit"),
                super::offset::credit(self.due_minor, currency, lang)
            );
        }
        for (amount, label) in [
            (
                self.incoming_minor,
                lang.pick("Verrechnetes Guthaben", "Credit applied"),
            ),
            (
                self.outgoing_minor,
                lang.pick("Eingesetztes Guthaben", "Credit used"),
            ),
        ] {
            if amount > 0 {
                let _ = write!(line, "<br>{label} {}", price(amount));
            }
        }
        if let Some(previous) = &self.predecessor {
            let _ = write!(
                line,
                "<br>{} {}",
                lang.pick("Vorgängerbeleg", "Preceding document"),
                escape(previous)
            );
        }
        line
    }
}

impl Balance {
    pub(super) fn matches(&self, query: &Scope) -> bool {
        self.scope == *query && self.offsets.iter().all(|r| r.belongs_to(query))
    }

    pub(super) fn render(&self, lang: Lang, page: u32, can_write: bool) -> Response {
        let price = |amount| money(amount, self.scope.currency, lang);
        let mut out = format!(
            r#"<p>{issuer} · {account}</p><div class="stats card"><div class="stat">{net}<strong class="value">{balance}</strong></div><div class="stat">{available}<strong class="value">{cash}</strong></div><div class="stat">{credit}<strong class="value">{credit_amount}</strong></div></div>"#,
            issuer = escape(&self.issuer),
            account = escape(&self.scope.account),
            net = lang.pick("Offener Saldo", "Net outstanding"),
            balance = price(self.net_due_minor),
            credit = lang.pick("Guthaben aus Gutschriften", "Credit on account"),
            credit_amount = price(self.credit_minor),
            available = lang.pick("Nicht zugeordneter Betrag", "Unapplied cash"),
            cash = price(self.unapplied_minor)
        );
        out.push_str(&row_section(
            lang.pick("Abrechnungsbelege", "Billing documents"),
            self.documents
                .iter()
                .map(|doc| doc.row(self.scope.currency, lang)),
            lang,
        ));
        out.push_str(&row_section(
            lang.pick("Zahlungseingänge", "Incoming payments"),
            self.payments.iter().map(|p| {
                format!(
                    "<strong>{}</strong> · {}<br>{} · {} {}",
                    escape(&p.reference),
                    escape(&p.source),
                    price(p.amount_minor),
                    lang.pick("Verfügbar", "Available"),
                    price(p.unapplied_minor)
                )
            }),
            lang,
        ));
        if self.can_offset && can_write {
            let candidates = self
                .documents
                .iter()
                .map(|d| (d.number.as_str(), d.due_minor));
            out.push_str(&super::offset::form(&self.scope, candidates, lang));
        }
        if self.can_allocate {
            out.push_str(&self.allocation_form(lang));
        }
        if !self.held.is_empty() {
            out.push_str(&row_section(
                lang.pick("Zur Prüfung", "Held for review"),
                self.held.iter().map(|row| {
                    format!(
                        "<strong>{}</strong> · {}<br>{} · {}",
                        reason(&row.reason, lang),
                        price(row.amount_minor),
                        escape(&row.source),
                        escape(row.reference.as_deref().unwrap_or("—"))
                    )
                }),
                lang,
            ));
        }
        out.push_str(&row_section(
            lang.pick("Gebuchte Zuordnungen", "Posted allocations"),
            self.allocations.iter().map(|a| {
                format!(
                    "<strong>{} · {}</strong><br>{} · {} · {}",
                    price(a.amount_minor),
                    escape(&a.number),
                    escape(&a.source),
                    escape(&a.reference),
                    escape(&a.actor)
                )
            }),
            lang,
        ));
        if !self.offsets.is_empty() {
            out.push_str(&row_section(
                lang.pick("Gebuchte Verrechnungen", "Posted credit offsets"),
                self.offsets.iter().map(|r| r.history(lang)),
                lang,
            ));
        }
        self.pagination(&mut out, lang, page);
        out.push_str(&back(lang));
        shell(lang, lang.pick("Kontostand", "Account balance"), &out)
    }

    fn pagination(&self, out: &mut String, lang: Lang, page: u32) {
        if page > 0 || self.next_page.is_some() {
            let _ = write!(
                out,
                r#"<nav class="card-controls" aria-label="{}">"#,
                lang.pick("Kontoseiten", "Account pages")
            );
            if page > 0 {
                out.push_str(&balance_button(
                    lang,
                    &self.scope,
                    page - 1,
                    lang.pick("Zurück", "Previous page"),
                ));
            }
            if let Some(next) = self.next_page {
                out.push_str(&balance_button(
                    lang,
                    &self.scope,
                    next,
                    lang.pick("Weiter", "Next page"),
                ));
            }
            out.push_str("</nav>");
        }
    }

    fn allocation_form(&self, lang: Lang) -> String {
        let payments: Vec<_> = self
            .payments
            .iter()
            .filter(|p| p.unapplied_minor > 0)
            .collect();
        let sources: BTreeSet<_> = payments.iter().map(|p| p.source.as_str()).collect();
        let mut lists = String::new();
        for (id, values) in [
            ("sources", sources.iter().copied().collect::<Vec<_>>()),
            (
                "payments",
                payments.iter().map(|p| p.reference.as_str()).collect(),
            ),
            (
                "documents",
                self.documents
                    .iter()
                    .filter(|d| d.open_minor > 0)
                    .map(|d| d.number.as_str())
                    .collect(),
            ),
        ] {
            let _ = write!(lists, r#"<datalist id="{id}">"#);
            for value in values {
                let _ = write!(lists, r#"<option value="{}"></option>"#, escape(value));
            }
            lists.push_str("</datalist>");
        }
        let source = if sources.len() == 1 {
            sources.first().copied().unwrap_or_default()
        } else {
            ""
        };
        format!(
            r#"<section class="card"><h2>{heading}</h2><form method="post" action="/receivables/review?lang={tag}" autocomplete="off">{scope}{lists}
<p><label>{source_label}<br><input type="text" name="source" list="sources" value="{source}" maxlength="32" required></label></p>
<p><label>{payment}<br><input type="text" name="reference" list="payments" maxlength="96" required></label></p>
<p><label>{document}<br><input type="text" name="number" list="documents" maxlength="128" required></label></p>
<p><label>{amount} ({currency})<br><input type="text" name="amount" inputmode="decimal" maxlength="32" required></label></p><button class="btn">{review}</button></form></section>"#,
            heading = lang.pick("Zahlung zuordnen", "Allocate a payment"),
            tag = lang.tag(),
            scope = scope_fields(&self.scope),
            source_label = lang.pick("Auszugskonto", "Statement source"),
            source = escape(source),
            payment = lang.pick("Buchungsreferenz", "Payment reference"),
            document = lang.pick("Rechnungsnummer", "Invoice number"),
            amount = lang.pick("Betrag", "Amount"),
            currency = self.scope.currency,
            review = lang.pick("Zuordnung prüfen", "Review allocation")
        )
    }
}

#[derive(Deserialize)]
pub(super) struct Imported {
    booked: usize,
    duplicates: usize,
    held: Vec<ImportHold>,
}

#[derive(Deserialize)]
struct ImportHold {
    row: usize,
    reason: String,
}

impl Imported {
    pub(super) fn render(&self, lang: Lang, scope: &Scope) -> Response {
        let mut body = format!(
            r#"<p>{} · {}</p><dl class="kv"><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd></dl>"#,
            escape(&scope.account),
            scope.currency,
            lang.pick("Neu gebucht", "Newly booked"),
            self.booked,
            lang.pick("Bereits importiert", "Already imported"),
            self.duplicates,
            lang.pick("Zur Prüfung", "Held for review"),
            self.held.len()
        );
        if !self.held.is_empty() {
            body.push_str(&row_section(
                lang.pick("Einträge prüfen", "Entries to review"),
                self.held.iter().map(|h| {
                    format!(
                        "{} {} · {}",
                        lang.pick("Zeile", "Row"),
                        h.row,
                        reason(&h.reason, lang)
                    )
                }),
                lang,
            ));
        }
        body.push_str(&balance_button(
            lang,
            scope,
            0,
            lang.pick("Kontostand öffnen", "Open account balance"),
        ));
        shell(
            lang,
            lang.pick("Auszug importiert", "Statement imported"),
            &body,
        )
    }
}

fn subject_fields(a: &super::Selection) -> String {
    scope_fields(&a.scope)
        + &hidden("source", &a.source)
        + &hidden("reference", &a.reference)
        + &hidden("number", &a.number)
}

fn facts(a: &super::Selection, lang: Lang) -> String {
    format!(
        r#"<dl class="kv"><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd></dl>"#,
        lang.pick("Rechnungskonto", "Billing account"),
        escape(&a.scope.account),
        lang.pick("Auszugskonto", "Statement source"),
        escape(&a.source),
        lang.pick("Buchungsreferenz", "Payment reference"),
        escape(&a.reference),
        lang.pick("Rechnungsnummer", "Invoice number"),
        escape(&a.number)
    )
}

pub(super) fn edit(lang: Lang, draft: &Draft) -> Response {
    let body = format!(r#"{}<form method="post" action="/receivables/review?lang={}">{}<p role="alert">{}</p><p><label>{} ({})<br><input name="amount" type="text" inputmode="decimal" value="{}" maxlength="32" required></label></p><button class="btn">{}</button></form>{}"#,
        facts(&draft.selection, lang), lang.tag(), subject_fields(&draft.selection),
        lang.pick("Einen positiven Betrag ohne Tausendertrennzeichen in der angegebenen Währung eingeben.", "Enter a positive amount in this currency without thousands separators."),
        lang.pick("Betrag", "Amount"), draft.selection.scope.currency, escape(&draft.amount),
        lang.pick("Zuordnung prüfen", "Review allocation"), back(lang));
    with_status(
        shell(lang, lang.pick("Betrag prüfen", "Check the amount"), &body),
        StatusCode::BAD_REQUEST,
    )
}

pub(super) fn decision(lang: Lang, a: &Allocation, result: &Preview, posted: bool) -> Response {
    let money = |amount| money(amount, a.selection.scope.currency, lang);
    let mut body = format!(
        r#"{}<dl class="kv"><dt>{}</dt><dd><strong>{}</strong></dd><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd></dl>"#,
        facts(&a.selection, lang),
        lang.pick("Zuordnungsbetrag", "Allocation amount"),
        money(a.amount_minor),
        lang.pick(
            "Offene Rechnung nach dieser Zuordnung",
            "Invoice balance after this allocation"
        ),
        money(result.open_after_minor),
        lang.pick(
            "Verfügbare Zahlung nach dieser Zuordnung",
            "Payment left after this allocation"
        ),
        money(result.unapplied_after_minor)
    );
    if !posted {
        let _ = write!(
            body,
            r#"<form method="post" action="/receivables/confirm?lang={}">{}{}{}<button class="btn">{}</button></form>"#,
            lang.tag(),
            subject_fields(&a.selection),
            hidden("amount_minor", &a.amount_minor.to_string()),
            hidden("review", &result.review),
            lang.pick("Zuordnung buchen", "Confirm allocation")
        );
    }
    body.push_str(&balance_button(
        lang,
        &a.selection.scope,
        0,
        if posted {
            lang.pick("Kontostand öffnen", "Open account balance")
        } else {
            lang.pick("Zurück zum Kontostand", "Back to account balance")
        },
    ));
    shell(
        lang,
        if posted {
            lang.pick("Zuordnung gebucht", "Allocation posted")
        } else {
            lang.pick("Zuordnung prüfen", "Review allocation")
        },
        &body,
    )
}
