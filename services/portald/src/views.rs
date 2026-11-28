//! Customer-facing HTML. Each number displayed here was supplied by its owner.
use std::fmt::Write as _;

use axum::http::StatusCode;
use axum::response::Response;
use emob_service::ui::{self, Lang, Shell, escape};

use crate::client::{
    Agreement, Customer, DocumentKind, History, Invoice, Owners, Receipt, WithdrawalStatus,
};

fn shell(lang: Lang, title: &str, body: &str) -> Response {
    ui::html(ui::page(
        &Shell {
            title,
            lang,
            place: lang.pick("Kundenportal", "Customer portal"),
            reading: true,
            script: None,
        },
        body,
    ))
}

fn language(lang: Lang, page: u32) -> String {
    let (other, label) = match lang {
        Lang::En => ("de", "Deutsch"),
        Lang::De => ("en", "English"),
    };
    format!(
        r#"<a href="/customer/?lang={other}&amp;page={page}" hreflang="{other}" lang="{other}" class="btn ghost">{label}</a>"#
    )
}

fn logout(lang: Lang) -> String {
    format!(
        r#"<form method="post" action="/customer/logout?lang={}"><button class="btn ghost" type="submit">{}</button></form>"#,
        lang.tag(),
        lang.pick("Abmelden", "Sign out")
    )
}

pub(crate) fn sign_in(lang: Lang, message: Option<&str>) -> Response {
    let title = lang.pick("Anmelden", "Sign in");
    let alert = message
        .map(|s| format!(r#"<p role="alert">{}</p>"#, escape(s)))
        .unwrap_or_default();
    let mut body = format!(
        r#"<div class="head"><h1>{title}</h1>{}</div>{alert}"#,
        language(lang, 0)
    );
    let _ = write!(
        body,
        r#"<form class="card" method="post" action="/customer/login?lang={}"><p><label for="subject">{}</label><br><input id="subject" name="subject" type="text" autocomplete="username" autocapitalize="none" spellcheck="false" maxlength="64" required></p><p><label for="secret">{}</label><br><input id="secret" name="secret" type="password" autocomplete="current-password" maxlength="64" required></p><button class="btn" type="submit">{title}</button></form>"#,
        lang.tag(),
        lang.pick("Benutzerkennung", "User ID"),
        lang.pick("Zugangsschlüssel", "Access key")
    );
    shell(lang, title, &body)
}

pub(crate) fn problem(lang: Lang, status: StatusCode, signed_in: bool) -> Response {
    let (title, hint) = match status {
        StatusCode::FORBIDDEN => (
            lang.pick("Anfrage abgelehnt", "Request refused"),
            lang.pick(
                "Öffnen Sie das Kundenportal und senden Sie das Formular von dort.",
                "Open the customer portal and submit the form there.",
            ),
        ),
        StatusCode::BAD_REQUEST => (
            lang.pick("Eingaben prüfen", "Check your entry"),
            lang.pick(
                "Öffnen Sie das Formular erneut und prüfen Sie die Eingaben.",
                "Reopen the form and check your entries.",
            ),
        ),
        StatusCode::CONFLICT => (
            lang.pick("Kein aktueller Rechnungssaldo", "No current balance for this invoice"),
            lang.pick("Prüfen Sie neuere Belege in Ihrer Übersicht. Bei einer neuen Rechnung kann die Buchung noch ausstehen.", "Check your overview for later documents. A newly issued invoice may still be awaiting booking."),
        ),
        StatusCode::NOT_FOUND => (
            lang.pick("Dokument nicht verfügbar", "Document unavailable"),
            lang.pick(
                "Öffnen Sie die Übersicht, um Ihre verfügbaren Dokumente zu sehen.",
                "Open your overview to see the documents available to you.",
            ),
        ),
        _ => (
            lang.pick(
                "Dienst vorübergehend nicht erreichbar",
                "Service temporarily unavailable",
            ),
            lang.pick(
                "Versuchen Sie es in Kürze erneut.",
                "Please try again shortly.",
            ),
        ),
    };
    let mut body = format!(
        r#"<h1>{title}</h1><p>{hint}</p><p class="back"><a href="/customer/?lang={}">{}</a></p>"#,
        lang.tag(),
        lang.pick("Zum Kundenportal", "Return to customer portal")
    );
    if signed_in {
        body.push_str(&logout(lang));
    }
    let mut response = shell(lang, title, &body);
    *response.status_mut() = status;
    response
}

fn field(body: &mut String, name: &str, value: &str) {
    let _ = write!(body, "<dt>{}</dt><dd>{}</dd>", escape(name), escape(value));
}

fn agreement(body: &mut String, a: &Agreement, lang: Lang, owners: &Owners) {
    let _ = write!(
        body,
        r#"<article class="card"><h3>{}</h3><dl class="kv">"#,
        escape(&a.emaid)
    );
    field(body, lang.pick("Ladepunkt", "Charging point"), &a.point);
    field(body, lang.pick("Tarif", "Plan"), &a.plan);
    field(
        body,
        lang.pick("Vertragsreferenz", "Agreement reference"),
        &a.id,
    );
    if let Some(end) = &a.ended_at {
        field(body, lang.pick("Vertragsende", "Ended at"), end);
    }
    if let Some(status) = a.withdrawal_status {
        let value = match status {
            WithdrawalStatus::Received => lang.pick("Eingegangen", "Received"),
            WithdrawalStatus::Held => lang.pick("In Prüfung", "Under review"),
            WithdrawalStatus::Approved => lang.pick("Anerkannt", "Approved"),
            WithdrawalStatus::Declined => lang.pick("Abgelehnt", "Declined"),
        };
        field(body, lang.pick("Widerruf", "Withdrawal"), value);
    }
    body.push_str("</dl>");
    if let Some(url) = owners.terms(&a.reference) {
        let _ = write!(
            body,
            r#"<p class="back"><a href="{}?lang={}">{}</a></p>"#,
            escape(&url),
            lang.tag(),
            lang.pick("Vereinbarte Preise ansehen", "View agreed prices")
        );
    }
    body.push_str("</article>");
}

fn invoice(body: &mut String, item: &Invoice, lang: Lang, balances: bool, page: u32) {
    let kind = match item.kind {
        DocumentKind::Invoice => lang.pick("Rechnung", "Invoice"),
        DocumentKind::Credit => lang.pick("Stornobeleg", "Credit note"),
        DocumentKind::LineCredit => lang.pick("Positionsgutschrift", "Line credit"),
        DocumentKind::Replacement => lang.pick("Ersatzrechnung", "Replacement invoice"),
    };
    let _ = write!(
        body,
        r#"<article class="card bill"><h3>{kind} · {}</h3><p class="value num">{} {}</p><dl class="kv">"#,
        escape(&item.period),
        escape(&if lang == Lang::De {
            item.payable.replace('.', ",")
        } else {
            item.payable.clone()
        }),
        escape(&item.currency)
    );
    field(
        body,
        lang.pick("Dokumentnummer", "Document number"),
        &item.number,
    );
    field(
        body,
        lang.pick("Rechnungskonto", "Billing account"),
        &item.account,
    );
    if let Some(replaced) = &item.supersedes {
        field(
            body,
            lang.pick("Vorherige Rechnung", "Previous invoice"),
            replaced,
        );
    }
    let _ = write!(
        body,
        r#"</dl><form method="post" action="/customer/document?lang={}"><input type="hidden" name="number" value="{}"><p><button class="btn" type="submit" aria-label="{} {}">{}</button></p></form>"#,
        lang.tag(),
        escape(&item.number),
        lang.pick("UBL herunterladen", "Download UBL"),
        escape(&item.number),
        lang.pick("UBL herunterladen", "Download UBL")
    );
    if balances && !matches!(item.kind, DocumentKind::Credit) {
        let number = match item.kind {
            DocumentKind::LineCredit => item.supersedes.as_deref(),
            _ => Some(item.number.as_str()),
        };
        if let Some(number) = number {
            let label = lang.pick("Rechnungssaldo ansehen", "View invoice balance");
            let _ = write!(
                body,
                r#"<form method="post" action="/customer/balance?lang={}&amp;page={page}"><input type="hidden" name="number" value="{number}"><button class="btn ghost" type="submit" aria-label="{label} · {number}">{label}</button></form>"#,
                lang.tag(),
                number = escape(number),
            );
        }
    }
    body.push_str("</article>");
}

pub(crate) fn balance(b: &crate::client::Balance, lang: Lang, page: u32) -> Response {
    let title = lang.pick("Rechnungssaldo", "Invoice balance");
    let mut body = format!(
        r#"<div class="head"><h1>{title}</h1></div><section class="card"><h2>{}</h2><dl class="kv">"#,
        escape(&b.number)
    );
    let digits = b.currency.minor_unit_digits();
    let scale = 10_u64.pow(digits);
    let mut amount = |label: &str, magnitude: u64| {
        let whole = magnitude / scale;
        let value = if digits == 0 {
            whole.to_string()
        } else {
            format!(
                "{whole}{}{:0width$}",
                lang.pick(",", "."),
                magnitude % scale,
                width = digits as usize
            )
        };
        field(&mut body, label, &format!("{value} {}", b.currency));
    };
    amount(
        lang.pick("Rechnungsbetrag", "Original amount"),
        b.amount_minor,
    );
    amount(
        lang.pick("Positionsgutschriften", "Line credits"),
        b.credited_minor,
    );
    amount(
        lang.pick("Zugeordnete Zahlungen", "Payments applied"),
        b.paid_minor,
    );
    if b.incoming_minor > 0 {
        amount(
            lang.pick(
                "Guthaben aus anderen Rechnungen",
                "Credit from other invoices",
            ),
            b.incoming_minor,
        );
    }
    if b.outgoing_minor > 0 {
        amount(
            lang.pick(
                "Guthaben für andere Rechnungen verwendet",
                "Credit used on other invoices",
            ),
            b.outgoing_minor,
        );
    }
    let remaining = if b.due_minor < 0 {
        lang.pick("Guthaben", "Credit balance")
    } else {
        lang.pick("Offener Betrag", "Amount due")
    };
    amount(remaining, b.due_minor.unsigned_abs());
    let _ = write!(
        body,
        r#"</dl></section><p><a class="btn ghost" href="/customer/?lang={}&amp;page={page}">{}</a></p>"#,
        lang.tag(),
        lang.pick("Zur Übersicht", "Back to overview")
    );
    shell(lang, title, &body)
}

fn render_receipt(body: &mut String, receipt: &Receipt, lang: Lang, owners: &Owners) {
    let end = receipt.ended_at.to_offset(time::UtcOffset::UTC);
    let energy = if lang == Lang::De {
        receipt.kwh.replace('.', ",")
    } else {
        receipt.kwh.clone()
    };
    let _ = write!(
        body,
        r#"<article class="card"><h3>{} · {} kWh</h3><dl class="kv">"#,
        lang.date(end.date()),
        escape(&energy)
    );
    field(
        body,
        lang.pick("Ladepunkt", "Charging point"),
        &receipt.point,
    );
    field(
        body,
        lang.pick("Ende", "Ended at"),
        &format!("{:02}:{:02} UTC", end.hour(), end.minute()),
    );
    field(
        body,
        lang.pick("Ladevorgang", "Charging record"),
        &receipt.source,
    );
    field(
        body,
        lang.pick("Vertragsreferenz", "Agreement reference"),
        &receipt.agreement,
    );
    if owners.receipt_allowed(&receipt.link) {
        let _ = write!(
            body,
            r#"</dl><p class="back"><a href="{}">{}</a></p>"#,
            escape(&receipt.link),
            lang.pick("Ladebeleg öffnen", "Open charging receipt")
        );
    } else {
        let _ = write!(
            body,
            "</dl><p>{}</p>",
            lang.pick("Beleglink nicht verfügbar.", "Receipt link unavailable.")
        );
    }
    body.push_str("</article>");
}

fn pagination(body: &mut String, p: u32, next: Option<u32>, lang: Lang) {
    if p == 0 && next.is_none() {
        return;
    }
    let _ = write!(
        body,
        r#"<nav class="card-controls" aria-label="{}">"#,
        lang.pick("Verlauf durchblättern", "Browse history")
    );
    for (page, label) in [
        (p.checked_sub(1), lang.pick("Zurück", "Previous")),
        (next, lang.pick("Weiter", "Next")),
    ] {
        if let Some(number) = page {
            let _ = write!(
                body,
                r#"<a class="btn ghost" href="/customer/?lang={}&amp;page={number}">{label}</a>"#,
                lang.tag()
            );
        }
    }
    body.push_str("</nav>");
}

pub(crate) fn overview(
    lang: Lang,
    customer: &Customer,
    history: Option<&History>,
    p: u32,
    owners: &Owners,
) -> Response {
    let title = lang.pick("Meine Übersicht", "My overview");
    let mut body = format!(
        r#"<div class="head"><h1>{title}</h1><div class="card-controls noprint">{}{}</div></div><p>{} <b>{}</b></p>"#,
        language(lang, p),
        logout(lang),
        lang.pick("Angemeldet als", "Signed in as"),
        escape(&customer.subject)
    );
    let _ = write!(
        body,
        "<section><h2>{}</h2>",
        lang.pick("Ladeverträge", "Charging agreements")
    );
    if customer.agreements.is_empty() {
        body.push_str(lang.pick(
            "<p>Keine Ladeverträge freigegeben.</p>",
            "<p>No charging agreements have been shared with you.</p>",
        ));
    }
    for a in &customer.agreements {
        agreement(&mut body, a, lang, owners);
    }
    body.push_str("</section>");
    let Some(history) = history else {
        let _ = write!(
            body,
            r#"<section class="card"><h2>{}</h2><p>{}</p><a class="btn ghost" href="/customer/?lang={}&amp;page={p}">{}</a></section>"#,
            lang.pick(
                "Abrechnung nicht erreichbar",
                "Billing temporarily unavailable"
            ),
            lang.pick(
                "Rechnungen und Ladebelege können derzeit nicht geladen werden.",
                "Invoices and charging receipts could not be loaded."
            ),
            lang.tag(),
            lang.pick("Erneut laden", "Try again")
        );
        let mut response = shell(lang, title, &body);
        *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
        return response;
    };
    if !customer.accounts.is_empty() || !history.invoices.is_empty() {
        let _ = write!(
            body,
            "<section><h2>{}</h2>",
            lang.pick("Rechnungen", "Invoices")
        );
        if history.invoices.is_empty() {
            body.push_str(lang.pick(
                "<p>Keine Rechnungen auf dieser Seite.</p>",
                "<p>No invoices on this page.</p>",
            ));
        }
        for item in &history.invoices {
            invoice(&mut body, item, lang, owners.accounting.is_some(), p);
        }
        body.push_str("</section>");
    }
    if !customer.agreements.is_empty() || !history.receipts.is_empty() {
        let _ = write!(
            body,
            "<section><h2>{}</h2>",
            lang.pick("Ladebelege", "Charging receipts")
        );
        if history.receipts.is_empty() {
            body.push_str(lang.pick(
                "<p>Keine Ladebelege auf dieser Seite.</p>",
                "<p>No charging receipts on this page.</p>",
            ));
        }
        for receipt in &history.receipts {
            render_receipt(&mut body, receipt, lang, owners);
        }
        body.push_str("</section>");
    }
    if customer.accounts.is_empty() && customer.agreements.is_empty() {
        body.push_str(lang.pick(
            "<p>Ihre Freigaben verwaltet Ihr Mobilitätsanbieter.</p>",
            "<p>Your mobility provider manages your access.</p>",
        ));
    }
    pagination(&mut body, p, history.next_page, lang);
    shell(lang, title, &body)
}
