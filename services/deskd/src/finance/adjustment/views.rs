use std::{collections::BTreeSet, fmt::Write as _};

use axum::{http::StatusCode, response::Response};
use emob_core::Currency;
use emob_service::ui::{Lang, escape};

use super::{Archive, Decision, Draft, Eligibility, Invoice, Kind, Reason, Stage};
use crate::fetch::Why;
use crate::finance::Scope;
use crate::finance::views::{balance_button, hidden, money};
use crate::investigation::{shell, valid_reference, with_status};
use crate::retail;

pub(crate) fn title(lang: Lang) -> &'static str {
    lang.pick("Rechnungsposition gutschreiben", "Credit an invoice line")
}

fn number_label(lang: Lang) -> &'static str {
    lang.pick("Originalrechnung", "Original invoice")
}

fn agreement_label(lang: Lang) -> &'static str {
    lang.pick("Vertragsreferenz", "Agreement reference")
}

fn line_label(lang: Lang) -> &'static str {
    lang.pick("Rechnungsposition", "Invoice line")
}

fn open_label(lang: Lang) -> &'static str {
    lang.pick("Vertragspositionen öffnen", "Open agreement lines")
}

fn lookup_button(invoice: &Invoice, page: u32, label: &str, lang: Lang) -> String {
    format!(
        r#"<form method="post" action="/line-credits/lookup?lang={}">{}{}<input type="hidden" name="page" value="{page}"><button class="btn ghost">{label}</button></form>"#,
        lang.tag(),
        hidden("number", &invoice.number),
        hidden("agreement", &invoice.agreement)
    )
}

pub(super) fn lookup_form(lang: Lang) -> String {
    format!(
        r#"<form class="card" method="post" action="/line-credits/lookup?lang={}" autocomplete="off">
<p><label>{}<br><input type="text" name="number" required maxlength="96" spellcheck="false"></label></p>
<p><label>{}<br><input type="text" name="agreement" required minlength="64" maxlength="64" spellcheck="false"></label></p>
<button class="btn">{}</button></form>"#,
        lang.tag(),
        number_label(lang),
        agreement_label(lang),
        open_label(lang)
    )
}

pub(crate) fn open_invoice<'a>(
    number: &str,
    agreements: impl Iterator<Item = &'a str>,
    lang: Lang,
) -> String {
    let agreements: BTreeSet<_> = agreements.filter(|s| valid_reference(s)).collect();
    let mut options = String::new();
    for agreement in agreements {
        let _ = write!(
            options,
            r#"<option value="{agreement}">{agreement}</option>"#
        );
    }
    if options.is_empty() {
        return String::new();
    }
    format!(
        r#"<form class="card" method="post" action="/line-credits/lookup?lang={}"><h2>{}</h2>{}<p><label>{}<br><select name="agreement" required>{options}</select></label></p><button class="btn">{}</button></form>"#,
        lang.tag(),
        title(lang),
        hidden("number", number),
        agreement_label(lang),
        open_label(lang)
    )
}

fn row(html: &mut String, label: &str, value: &str) {
    let _ = write!(html, "<dt>{label}</dt><dd>{}</dd>", escape(value));
}

fn identity(invoice: &Invoice, account: &str, lang: Lang) -> String {
    let mut fields = String::new();
    row(&mut fields, number_label(lang), &invoice.number);
    row(&mut fields, retail::account_label(lang), account);
    row(&mut fields, agreement_label(lang), &invoice.agreement);
    fields
}

fn amount_field(value: &str, lang: Lang) -> String {
    format!(
        r#"<p><label>{}<br><input type="text" name="amount" value="{}" required maxlength="22" inputmode="decimal" aria-describedby="amount-help"></label><br><small id="amount-help">{}</small></p>"#,
        lang.pick("Nettogutschrift (EUR)", "Net credit (EUR)"),
        escape(value),
        lang.pick(
            "Positiven Betrag mit höchstens zwei Nachkommastellen eingeben.",
            "Enter a positive amount with up to two decimal places."
        )
    )
}

fn line_choices(bill: &Archive, lang: Lang) -> (String, String) {
    let mut options = String::new();
    let mut evidence = String::new();
    for line in &bill.lines {
        let id = escape(&line.id);
        let kind = match line.kind {
            Kind::Monthly => lang.pick("Monatsbeitrag", "Monthly fee"),
            Kind::Session => lang.pick("Ladevorgang", "Charging session"),
        };
        let remaining = money(line.remaining_minor, bill.currency, lang);
        if matches!(bill.state, Eligibility::Available) && line.remaining_minor > 0 {
            let _ = write!(
                options,
                r#"<option value="{id}">{id} · {kind} · {remaining}</option>"#
            );
        }
        let _ = write!(
            evidence,
            r#"<li><strong>{id} · {kind}</strong><dl class="kv">"#
        );
        row(&mut evidence, lang.pick("Quelle", "Source"), &line.source);
        row(
            &mut evidence,
            lang.pick("Ursprüngliches Netto", "Original net"),
            &money(line.net_minor, bill.currency, lang),
        );
        row(
            &mut evidence,
            lang.pick("Umsatzsteuersatz", "VAT rate"),
            &format!("{} %", lang.decimal(line.vat_rate)),
        );
        row(
            &mut evidence,
            lang.pick("Noch gutschreibbar (netto)", "Remaining creditable net"),
            &remaining,
        );
        evidence.push_str("</dl>");
        if let Some(credit) = &line.last_credit {
            let label = format!(
                "{} · {credit}",
                lang.pick(
                    "Letzte Gutschrift laden (UBL)",
                    "Download latest credit (UBL)"
                )
            );
            evidence.push_str(&retail::button(
                "document",
                &bill.account,
                &bill.period,
                Some(("number", credit)),
                &escape(&label),
                lang,
            ));
        }
        evidence.push_str("</li>");
    }
    (options, evidence)
}

pub(super) fn lines(bill: &Archive, page: u32, lang: Lang) -> Response {
    let mut body = format!(
        r#"<dl class="kv">{}</dl>"#,
        identity(&bill.invoice, &bill.account, lang)
    );
    let (options, evidence) = line_choices(bill, lang);
    if options.is_empty() {
        let _ = write!(
            body,
            "<p>{}</p>",
            lang.pick(
                "Auf dieser Seite ist keine weitere Gutschrift möglich.",
                "No further credit is available on this page."
            )
        );
    } else {
        let mut reasons = String::new();
        for reason in [
            Reason::BillingError,
            Reason::ServiceDispute,
            Reason::Goodwill,
        ] {
            let _ = write!(
                reasons,
                r#"<option value="{}">{}</option>"#,
                reason.code(),
                reason.label(lang)
            );
        }
        let _ = write!(
            body,
            r#"<form class="card" method="post" action="/line-credits/review?lang={tag}" autocomplete="off">
{number}{agreement}<p><label>{label}<br><select name="line_id" required>{options}</select></label></p>
{amount}<p><label>{why}<br><select name="reason" required><option value="">{select}</option>{reasons}</select></label></p>
<button class="btn">{review}</button></form>"#,
            tag = lang.tag(),
            number = hidden("number", &bill.invoice.number),
            agreement = hidden("agreement", &bill.invoice.agreement),
            label = lang.pick(
                "Position · noch gutschreibbar (netto)",
                "Line · remaining creditable net"
            ),
            amount = amount_field("", lang),
            why = lang.pick("Grund", "Reason"),
            select = lang.pick("Grund auswählen", "Select a reason"),
            review = lang.pick("Gutschrift prüfen", "Review credit")
        );
    }
    if !evidence.is_empty() {
        let _ = write!(
            body,
            r#"<section class="card"><h2>{}</h2><ul>{evidence}</ul></section>"#,
            lang.pick("Positionsnachweise", "Line evidence")
        );
    }
    for (target, label) in [
        (
            page.checked_sub(1),
            lang.pick("Vorherige Positionen", "Previous lines"),
        ),
        (
            bill.next_page,
            lang.pick("Weitere Positionen", "More lines"),
        ),
    ] {
        if let Some(p) = target {
            body.push_str(&lookup_button(&bill.invoice, p, label, lang));
        }
    }
    shell(
        lang,
        lang.pick(
            "Rechnungspositionen dieses Vertrags",
            "Invoice lines for this agreement",
        ),
        &body,
    )
}

pub(super) fn edit_amount(draft: &Draft, lang: Lang) -> Response {
    let invoice = &draft.choice.invoice;
    let fields = hidden("number", &invoice.number)
        + &hidden("agreement", &invoice.agreement)
        + &hidden("line_id", &draft.choice.line_id)
        + &hidden("reason", draft.choice.reason.code());
    let body = format!(
        r#"<form class="card" method="post" action="/line-credits/review?lang={}">{fields}<p role="alert">{}</p><p>{} · {}</p>{}<button class="btn">{}</button></form>"#,
        lang.tag(),
        lang.pick("Betrag prüfen.", "Check the amount."),
        escape(&invoice.number),
        escape(&draft.choice.line_id),
        amount_field(&draft.amount, lang),
        lang.pick("Gutschrift prüfen", "Review credit")
    );
    with_status(shell(lang, title(lang), &body), StatusCode::BAD_REQUEST)
}

pub(super) fn decision(out: &Decision, lang: Lang) -> Response {
    let invoice = &out.choice.invoice;
    let mut fields = identity(invoice, &out.account, lang);
    row(&mut fields, line_label(lang), &out.choice.line_id);
    row(
        &mut fields,
        lang.pick("Grund", "Reason"),
        out.choice.reason.label(lang),
    );
    row(
        &mut fields,
        lang.pick("Nettogutschrift", "Credit net"),
        &money(out.net_minor, out.currency, lang),
    );
    let tax = format!(
        "{} ({} %)",
        lang.pick("Umsatzsteuer", "VAT"),
        lang.decimal(out.vat_rate)
    );
    row(&mut fields, &tax, &money(out.tax_minor, out.currency, lang));
    row(
        &mut fields,
        lang.pick(
            "Danach noch gutschreibbar (netto)",
            "Remaining creditable net after issue",
        ),
        &money(out.remaining_minor, out.currency, lang),
    );
    let mut body = format!(
        r#"<section class="card"><h2>{}</h2><p class="stat"><strong class="value">{}</strong></p><dl class="kv">{fields}</dl></section>"#,
        lang.pick(
            "Gutschrift einschließlich Umsatzsteuer",
            "Credit including VAT"
        ),
        money(out.gross_minor, out.currency, lang)
    );
    let heading = match out.state {
        Stage::Ready => {
            let command = hidden("number", &invoice.number)
                + &hidden("agreement", &invoice.agreement)
                + &hidden("line_id", &out.choice.line_id)
                + &hidden("reason", out.choice.reason.code())
                + &hidden("net_minor", &out.net_minor.to_string())
                + &hidden("review", &out.review);
            let _ = write!(body,
                r#"<p>{effect}</p><div class="card-controls"><form method="post" action="/line-credits/confirm?lang={tag}">{command}<button class="btn">{issue}</button></form>{edit}</div>"#,
                effect = lang.pick(
                    "Die Gutschrift mindert den Kontosaldo um den Bruttobetrag. Bereits zugeordnete Zahlungen bleiben auf der Originalrechnung.",
                    "The gross credit reduces the billing account balance. Recorded payments stay on the original invoice.",
                ),
                tag = lang.tag(),
                issue = lang.pick("Gutschrift verbindlich ausstellen", "Confirm and issue credit"),
                edit = lookup_button(invoice, 0, lang.pick("Auswahl ändern", "Change selection"), lang),
            );
            lang.pick("Positionsgutschrift prüfen", "Review line credit")
        }
        Stage::Issued => {
            if let Some(credit) = &out.credit {
                let _ = write!(
                    body,
                    "<p>{} <strong>{}</strong></p>",
                    lang.pick("Nummer der Gutschrift", "Credit number"),
                    escape(credit)
                );
                body.push_str(&retail::button(
                    "document",
                    &out.account,
                    &out.period,
                    Some(("number", credit)),
                    lang.pick("Gutschrift laden (UBL)", "Download credit (UBL)"),
                    lang,
                ));
            }
            body.push_str(&lookup_button(invoice, 0, open_label(lang), lang));
            body.push_str(&balance_button(
                lang,
                &Scope {
                    account: out.account.clone(),
                    currency: Currency::EUR,
                },
                0,
                lang.pick("Kontostand öffnen", "Open account balance"),
            ));
            lang.pick("Positionsgutschrift ausgestellt", "Line credit issued")
        }
    };
    shell(lang, heading, &body)
}

pub(in crate::finance) fn problem(lang: Lang, why: Why, invoice: Option<&Invoice>) -> Response {
    let (status,message)=match why {
        Why::Refused => (StatusCode::FORBIDDEN,lang.pick("Ein Finanzzugang mit Schreibrecht und eine Übermittlung direkt aus Heute sind erforderlich.","Use a finance account with write access and submit the form from Heute.")),
        Why::Status(400|413|415|422) => (StatusCode::BAD_REQUEST,lang.pick("Rechnungsnummer, Vertragsreferenz und Gutschriftsbetrag prüfen.","Check the invoice number, agreement reference and credit amount.")),
        Why::Status(404) => (StatusCode::NOT_FOUND,lang.pick("Keine passende Rechnungsposition für diesen Vertrag gefunden.","No matching invoice line was found for this agreement.")),
        Why::Status(409) => (StatusCode::CONFLICT,lang.pick("Die Position oder der verbleibende Betrag lässt diese Gutschrift nicht zu. Aktuelle Positionen erneut prüfen.","This line or its remaining amount does not allow this credit. Reopen the lines to review the current amounts.")),
        _ => (StatusCode::SERVICE_UNAVAILABLE,lang.pick("Der Abrechnungsdienst kann die Gutschrift zurzeit nicht bestätigen. Nach einem Ausstellungsversuch die letzte Gutschrift der Position prüfen.","Billing could not confirm the outcome. If you already submitted a credit, reopen the line to check its latest credit.")),
    };
    let back = invoice.map_or_else(
        || {
            format!(
                r#"<p><a href="/line-credits?lang={}">{}</a></p>"#,
                lang.tag(),
                title(lang)
            )
        },
        |i| lookup_button(i, 0, open_label(lang), lang),
    );
    with_status(
        shell(
            lang,
            title(lang),
            &format!(r#"<p role="alert">{message}</p>{back}"#),
        ),
        status,
    )
}
