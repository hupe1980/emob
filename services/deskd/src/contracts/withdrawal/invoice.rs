//! Open the issued document and its scoped payment evidence before reviewing a credit.
use std::fmt::Write as _;

use emob_core::Currency;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::{
    Arc, Extension, Form, Lang, PATH, Page, Response, Staff, State, Why, button, escape, hidden,
    json, open_case, problem, shell, valid_reference, views::row,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Selection {
    agreement: String,
    number: String,
    #[serde(default)]
    page: u32,
    #[serde(default)]
    cash_page: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Confirmation {
    agreement: String,
    number: String,
    review: String,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StateOfCredit {
    Evidence,
    Ready,
    Mixed,
    Superseded,
    LineAdjusted,
    Credited,
}

#[derive(Deserialize)]
struct Bill {
    agreement: String,
    account: String,
    number: String,
    period: String,
    currency: Currency,
    payable: Decimal,
    lines: Vec<Line>,
    state: StateOfCredit,
    review: Option<String>,
    credit: Option<String>,
    next_page: Option<u32>,
}

#[derive(Deserialize)]
struct Line {
    source: String,
    kind: Kind,
    gross: Decimal,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Session,
    Monthly,
}

#[derive(Deserialize)]
struct Cash {
    account: String,
    currency: Currency,
    number: String,
    open_minor: i64,
    allocations: Vec<Allocation>,
    next_page: Option<u32>,
}

#[derive(Deserialize)]
struct Allocation {
    number: String,
    source: String,
    reference: String,
    amount_minor: i64,
    actor: String,
}

fn valid_number(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 96
        && n.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".*/_-".contains(&c))
}

pub(super) fn find(agreement: &str, account: &str, lang: Lang) -> String {
    format!(
        r#"<section class="card"><h2>{title}</h2><form method="post" action="{PATH}/evidence?lang={tag}" autocomplete="off">{scope}<p><label>{number}<br><input name="number" type="text" maxlength="96" required spellcheck="false" aria-describedby="bill-help"></label><br><small id="bill-help">{help}</small></p><button class="btn ghost">{find}</button></form><details class="noprint"><summary>{by_month}</summary><form method="post" action="/retail-invoices/lookup?lang={tag}">{payer}<p><label>{period}<br><input type="month" name="period" required></label></p><button class="btn ghost">{lookup}</button></form></details></section>"#,
        title = lang.pick("Abrechnungsnachweise", "Billing evidence"),
        payer = hidden("account", account),
        by_month = lang.pick("Rechnungsnummer nachschlagen", "Find the invoice number"),
        period = lang.pick("Abrechnungsmonat", "Billing month"),
        lookup = lang.pick(
            "Ausgestellte Monatsrechnung öffnen",
            "Open issued monthly invoice"
        ),
        tag = lang.tag(),
        scope = hidden("agreement", agreement),
        number = lang.pick("Rechnungsnummer", "Invoice number"),
        help = lang.pick(
            "Die Nummer steht auf der ausgestellten Kundenrechnung.",
            "Use the number on the issued retail invoice."
        ),
        find = lang.pick("Rechnungspositionen prüfen", "Inspect invoice lines")
    )
}

fn pages(selection: &Selection, page: u32, cash_page: u32, label: &str, lang: Lang) -> String {
    let fields = hidden("agreement", &selection.agreement)
        + &hidden("number", &selection.number)
        + &hidden("page", &page.to_string())
        + &hidden("cash_page", &cash_page.to_string());
    button("/contracts/withdrawal/evidence", &fields, label, lang)
}

async fn cash_section(page: &Page, staff: &Staff, selection: &Selection, bill: &Bill) -> String {
    let lang = staff.lang;
    if !staff.finance {
        return format!(
            "<p>{}</p>",
            lang.pick(
                "Zahlungszuordnungen sind mit Finanzzugriff einsehbar.",
                "A finance grant is required to see payment allocations."
            )
        );
    }
    let unavailable = || {
        format!(
            r#"<p role="status">{}</p>"#,
            lang.pick(
                "Der Zahlungsstand ist derzeit nicht verfügbar.",
                "Payment evidence is currently unavailable."
            )
        )
    };
    let Some(url) = &page.accountingd else {
        return unavailable();
    };
    let url = format!(
        "{}/v1/accountingd/withdrawal/evidence",
        url.trim_end_matches('/')
    );
    let payload = json!({"account":bill.account,"currency":bill.currency,"number":bill.number,"page":selection.cash_page});
    let Ok(cash) = page
        .reader
        .operator_action::<Cash>(&url, &staff.token, &payload)
        .await
    else {
        return unavailable();
    };
    if cash.number != bill.number
        || cash.account != bill.account
        || cash.currency != bill.currency
        || cash.allocations.len() > 50
    {
        return unavailable();
    }
    let money = |minor: i64| {
        let amount = Decimal::new(minor, bill.currency.minor_unit_digits());
        format!("{} {}", lang.decimal(amount), bill.currency)
    };
    let mut body = format!(
        "<h2>{}</h2><dl class=\"kv\"><dt>{}</dt><dd>{}</dd></dl>",
        lang.pick("Zahlungszuordnungen", "Payment allocations"),
        lang.pick("Offener Rechnungsbetrag", "Open invoice amount"),
        money(cash.open_minor)
    );
    if cash.allocations.is_empty() {
        let message = lang.pick(
            "Für diesen Beleg liegen auf dieser Seite keine Zahlungszuordnungen vor.",
            "No allocations on this page for this document.",
        );
        let _ = write!(body, "<p>{message}</p>");
    } else {
        body.push_str("<ul>");
        for a in cash.allocations {
            let _ = write!(
                body,
                "<li><strong>{}</strong> · {} · {} · {} · {}</li>",
                money(a.amount_minor),
                escape(&a.source),
                escape(&a.reference),
                escape(&a.number),
                escape(&a.actor)
            );
        }
        body.push_str("</ul>");
    }
    for (p, label) in [
        (
            selection.cash_page.checked_sub(1),
            lang.pick("Vorherige Zuordnungen", "Previous allocations"),
        ),
        (
            cash.next_page,
            lang.pick("Weitere Zuordnungen", "More allocations"),
        ),
    ] {
        if let Some(p) = p {
            body.push_str(&pages(selection, selection.page, p, label, lang));
        }
    }
    body
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Evidence,
    Review,
    Confirm,
}

async fn show(
    page: &Page,
    staff: &Staff,
    selection: &Selection,
    action: Action,
    review: Option<&str>,
) -> Response {
    let lang = staff.lang;
    if action != Action::Evidence && !staff.finance {
        return problem(lang, Why::Refused, None);
    }
    if !valid_reference(&selection.agreement)
        || !valid_number(&selection.number)
        || review.is_some_and(|r| !valid_reference(r))
    {
        return problem(lang, Why::Status(400), None);
    }
    let mut input = json!({"agreement":selection.agreement,"number":selection.number});
    let operation = match action {
        Action::Evidence => {
            input["page"] = json!(selection.page);
            "evidence"
        }
        Action::Review => "review",
        Action::Confirm => {
            input["review"] = json!(review);
            "confirm"
        }
    };
    let url = format!(
        "{}/v1/billd/withdrawal/{operation}",
        page.billd.trim_end_matches('/')
    );
    let bill = match page
        .reader
        .operator_action::<Bill>(&url, &staff.token, &input)
        .await
    {
        Ok(bill)
            if (action != Action::Confirm || bill.state == StateOfCredit::Credited)
                && bill.agreement == selection.agreement
                && bill.number == selection.number
                && crate::retail::valid_account(&bill.account)
                && bill.lines.len() <= 50
                && bill.review.as_deref().is_none_or(valid_reference)
                && bill.credit.as_deref().is_none_or(valid_number) =>
        {
            bill
        }
        Ok(_) => return problem(lang, Why::Unreadable, Some(&selection.agreement)),
        Err(why) => return problem(lang, why, Some(&selection.agreement)),
    };
    let mut body = charge_lines(&bill, selection, lang);
    body.push_str("<section class=\"card\">");
    body.push_str(&cash_section(page, staff, selection, &bill).await);
    body.push_str("</section>");
    let Some(credit) = credit_section(&bill, action, staff.finance, lang) else {
        return problem(lang, Why::Unreadable, Some(&selection.agreement));
    };
    body.push_str(&credit);
    body.push_str(&open_case(&selection.agreement, lang));
    shell(
        lang,
        lang.pick("Rechnung zum Widerruf", "Withdrawal invoice evidence"),
        &body,
    )
}

fn charge_lines(bill: &Bill, selection: &Selection, lang: Lang) -> String {
    let mut rows = String::new();
    row(
        &mut rows,
        lang.pick("Rechnungsnummer", "Invoice number"),
        &bill.number,
    );
    row(
        &mut rows,
        lang.pick("Vertragsreferenz", "Agreement reference"),
        &bill.agreement,
    );
    row(
        &mut rows,
        lang.pick("Rechnungskonto", "Billing account"),
        &bill.account,
    );
    row(
        &mut rows,
        lang.pick("Abrechnungsmonat", "Billing month"),
        &bill.period,
    );
    row(
        &mut rows,
        lang.pick("Ausgestellter Gesamtbetrag", "Issued invoice total"),
        &format!("{} {}", lang.decimal(bill.payable), bill.currency),
    );
    let mut body = format!(
        r#"<section class="card"><h2>{}</h2><dl class="kv">{rows}</dl><h3>{}</h3><ul>"#,
        lang.pick("Ausgestellte Rechnung", "Issued invoice"),
        lang.pick("Positionen dieses Vertrags", "Lines for this agreement")
    );
    for l in &bill.lines {
        let kind = match l.kind {
            Kind::Session => lang.pick("Ladevorgang", "Charging session"),
            Kind::Monthly => lang.pick("Monatsbeitrag", "Monthly fee"),
        };
        let _ = write!(
            body,
            "<li><strong>{kind}</strong> · {} {} · <code>{}</code></li>",
            lang.decimal(l.gross),
            bill.currency,
            escape(&l.source)
        );
    }
    body.push_str("</ul>");
    for (p, label) in [
        (
            selection.page.checked_sub(1),
            lang.pick("Vorherige Positionen", "Previous lines"),
        ),
        (
            bill.next_page,
            lang.pick("Weitere Positionen", "More lines"),
        ),
    ] {
        if let Some(p) = p {
            body.push_str(&pages(selection, p, selection.cash_page, label, lang));
        }
    }
    body.push_str("</section>");
    body
}

fn credit_section(bill: &Bill, action: Action, finance: bool, lang: Lang) -> Option<String> {
    let mut body = String::from("<section class=\"card\">");
    let scope = hidden("agreement", &bill.agreement) + &hidden("number", &bill.number);
    let (title,info)=match bill.state {
        StateOfCredit::Credited => (lang.pick("Gutschrift ausgestellt","Credit issued"),lang.pick("Nummer der Gutschrift","Credit number")),
        StateOfCredit::LineAdjusted => (lang.pick("Positionsgutschrift vorhanden","Line credit already issued"),lang.pick("Prüfen Sie den noch gutschreibbaren Betrag der einzelnen Positionen.","Review the remaining creditable amount on each line.")),
        StateOfCredit::Mixed => (lang.pick("Gemeinsame Rechnung","Shared invoice"),lang.pick("Diese Rechnung enthält weitere Verträge. Eine vollständige Gutschrift würde auch deren Positionen erfassen.","This invoice includes other agreements. A full credit would also cover their charges.")),
        StateOfCredit::Superseded =>(lang.pick("Rechnung bereits berichtigt","Invoice already corrected"),lang.pick("Prüfen Sie die aktuelle Rechnungskette.","Check the current invoice chain.")),
        StateOfCredit::Ready => (lang.pick("Gutschrift prüfen","Review credit"),lang.pick("Die Bestätigung erstellt eine Gutschrift über die gesamte Rechnung. Bereits zugeordnete Zahlungen bleiben im Nebenbuch nachvollziehbar.","Confirmation issues a credit for the full invoice. Existing allocations remain recorded in receivables.")),
        StateOfCredit::Evidence => (lang.pick("Rechnungsprüfung","Invoice review"),lang.pick("Eine Gutschrift erfordert einen anerkannten Widerruf mit gesondertem Prüfbedarf für die Abrechnung.","A credit requires an approved withdrawal with a separate billing review.")),
    };
    let _ = write!(body, "<h2>{title}</h2><p>{info}</p>");
    match (action, bill.state) {
        (_, StateOfCredit::Credited) => {
            let credit = bill.credit.as_ref()?;
            let _ = write!(body, "<p><code>{}</code></p>", escape(credit));
            if !finance {
                return Some(body + "</section>");
            }
            let fields = hidden("account", &bill.account) + &hidden("period", &bill.period);
            for (number, label) in [
                (
                    &bill.number,
                    lang.pick(
                        "Originalrechnung laden (UBL)",
                        "Download original invoice (UBL)",
                    ),
                ),
                (
                    credit,
                    lang.pick("Gutschrift laden (UBL)", "Download credit (UBL)"),
                ),
            ] {
                body.push_str(&button(
                    "/retail-invoices/document",
                    &format!("{fields}{}", hidden("number", number)),
                    label,
                    lang,
                ));
            }
        }
        (Action::Evidence, StateOfCredit::Mixed | StateOfCredit::LineAdjusted) if finance => {
            body.push_str(&button(
                "/line-credits/lookup",
                &scope,
                lang.pick("Vertragspositionen öffnen", "Open agreement lines"),
                lang,
            ));
        }
        (Action::Review, StateOfCredit::Ready) => {
            let review = bill.review.as_ref()?;
            let fields = scope + &hidden("review", review);
            body.push_str(&button(
                "/contracts/withdrawal/credit-confirm",
                &fields,
                lang.pick(
                    "Gutschrift verbindlich ausstellen",
                    "Confirm and issue credit",
                ),
                lang,
            ));
        }
        (Action::Evidence, StateOfCredit::Ready | StateOfCredit::Evidence) if finance => {
            body.push_str(&button(
                "/contracts/withdrawal/credit-review",
                &scope,
                lang.pick("Gutschrift prüfen", "Review credit"),
                lang,
            ));
        }
        _ => {}
    }
    body.push_str("</section>");
    Some(body)
}

pub(super) async fn evidence(
    State(page): State<Arc<Page>>,
    Extension(staff): Extension<Staff>,
    Form(sel): Form<Selection>,
) -> Response {
    show(&page, &staff, &sel, Action::Evidence, None).await
}

pub(super) async fn review(
    State(page): State<Arc<Page>>,
    Extension(staff): Extension<Staff>,
    Form(sel): Form<Selection>,
) -> Response {
    show(&page, &staff, &sel, Action::Review, None).await
}

pub(super) async fn confirm(
    State(page): State<Arc<Page>>,
    Extension(staff): Extension<Staff>,
    Form(sel): Form<Confirmation>,
) -> Response {
    let selection = Selection {
        agreement: sel.agreement,
        number: sel.number,
        page: 0,
        cash_page: 0,
    };
    show(
        &page,
        &staff,
        &selection,
        Action::Confirm,
        Some(&sel.review),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issuing_a_full_credit_requires_a_finance_review_of_a_wholly_attributable_invoice() {
        let mut bill: Bill=serde_json::from_value(json!({"account":"PAYER-PRIVATE","agreement":"a".repeat(64),"number":"INV-1","period":"2026-11","currency":"EUR","payable":"10.20","lines":[],"state":"evidence"})).unwrap();
        assert!(
            !credit_section(&bill, Action::Evidence, false, Lang::En)
                .unwrap()
                .contains("<form")
        );
        for state in [
            StateOfCredit::Mixed,
            StateOfCredit::LineAdjusted,
            StateOfCredit::Superseded,
        ] {
            bill.state = state;
            assert!(
                !credit_section(&bill, Action::Evidence, true, Lang::De)
                    .unwrap()
                    .contains("credit-confirm")
            );
        }
        bill.state = StateOfCredit::Ready;
        assert!(credit_section(&bill, Action::Review, true, Lang::En).is_none());
        bill.review = Some("a".repeat(64));
        let reviewed = credit_section(&bill, Action::Review, true, Lang::En).unwrap();
        assert!(
            reviewed.contains("/credit-confirm?lang=en")
                && reviewed.contains("Confirm and issue credit")
        );
        bill.credit = Some("CREDIT-2".into());
        bill.state = StateOfCredit::Credited;
        let receipt = credit_section(&bill, Action::Confirm, true, Lang::En).unwrap();
        assert!(
            receipt.contains("CREDIT-2")
                && receipt.contains("Download credit (UBL)")
                && !receipt.contains("credit-confirm")
        );
    }
}
