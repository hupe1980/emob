//! Review and close one retail account's month using the billing owner's figures.
use std::fmt::Write as _;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use emob_service::authority::OperatorAccess;
use emob_service::estate::RetailAccountKind;
pub(super) use emob_service::estate::valid_account_id as valid_account;
use emob_service::ui::{Lang, escape};
use serde::{Deserialize, Serialize};

use crate::fetch::Why;
use crate::investigation::{Params, shell, valid_reference, with_status};
use crate::{Page, language, stations};

pub(super) fn routes() -> Router<Arc<Page>> {
    Router::new()
        .route("/retail-invoices", get(index))
        .route("/retail-invoices/{action}", post(submit))
        .layer(DefaultBodyLimit::max(2048))
}

#[derive(Deserialize)]
pub(super) struct AccountChoice {
    id: String,
    kind: RetailAccountKind,
}

#[derive(Deserialize)]
struct Accounts {
    accounts: Vec<AccountChoice>,
}

pub(super) fn account_label(lang: Lang) -> &'static str {
    lang.pick("Rechnungskonto", "Billing account")
}

pub(super) fn account_field(accounts: &[AccountChoice], lang: Lang) -> String {
    let mut field = format!(
        r#"<label>{}<br><select name="account" required><option value="">{}</option>"#,
        account_label(lang),
        lang.pick("Konto auswählen", "Select an account")
    );
    for choice in accounts.iter().filter(|choice| valid_account(&choice.id)) {
        let kind = match choice.kind {
            RetailAccountKind::Private => lang.pick("Privat", "Private"),
            RetailAccountKind::Fleet => lang.pick("Flotte", "Fleet"),
        };
        let _ = write!(
            field,
            r#"<option value="{id}">{id} · {kind}</option>"#,
            id = escape(&choice.id)
        );
    }
    field.push_str("</select></label>");
    format!("<p>{field}</p>")
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    account: String,
    period: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    review: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    number: Option<String>,
}

pub(super) fn valid_period(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 7
        && b[4] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && matches!(
            &b[5..],
            b"01"
                | b"02"
                | b"03"
                | b"04"
                | b"05"
                | b"06"
                | b"07"
                | b"08"
                | b"09"
                | b"10"
                | b"11"
                | b"12"
        )
}

fn title(lang: Lang) -> &'static str {
    lang.pick("Kundenrechnungen", "Retail invoices")
}

fn back(lang: Lang) -> String {
    format!(
        r#"<p class="back"><a href="/retail-invoices?lang={}">{}</a></p>"#,
        lang.tag(),
        lang.pick(
            "Anderen Abrechnungsmonat öffnen",
            "Open another billing period"
        )
    )
}

async fn index(
    State(desk): State<Arc<Page>>,
    Extension(access): Extension<OperatorAccess>,
    Query(params): Query<Params>,
    headers: HeaderMap,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    let accounts = desk
        .reader
        .fetch::<Accounts>(&format!(
            "{}/v1/empd/contracts/options",
            desk.empd.trim_end_matches('/')
        ))
        .await;
    let field = match accounts {
        Ok(list) if !list.accounts.is_empty() => account_field(&list.accounts, lang),
        _ => format!(
            r#"<p><label>{}<br><input name="account" required maxlength="32"></label></p><p>{}</p>"#,
            account_label(lang),
            lang.pick(
                "Geben Sie die Kontoreferenz ein, um eine bereits ausgestellte Rechnung zu öffnen.",
                "Enter the account reference to reopen a previously issued invoice."
            )
        ),
    };
    let mut body = format!(
        r#"<form method="post" action="/retail-invoices/review?lang={tag}" class="card" autocomplete="off">{field}<p><label>{period}<br><input type="month" name="period" required aria-describedby="period-help"></label><br><small id="period-help">{help}</small></p><div class="card-controls"><button class="btn">{review}</button><button class="btn ghost" formaction="/retail-invoices/lookup?lang={tag}">{lookup}</button></div></form>"#,
        tag = lang.tag(),
        period = lang.pick("Abrechnungsmonat", "Billing month"),
        help = lang.pick(
            "Wählen Sie einen abgeschlossenen Kalendermonat (UTC).",
            "Choose a completed calendar month (UTC)."
        ),
        review = lang.pick("Abrechnung prüfen", "Review charges"),
        lookup = lang.pick("Ausgestellte Rechnung öffnen", "Open issued invoice")
    );
    if access.can_write() && access.can_finance() {
        let _ = write!(
            body,
            r#"<p><a class="btn ghost" href="/line-credits?lang={}">{}</a></p>"#,
            lang.tag(),
            crate::finance::adjustment::title(lang)
        );
    }
    shell(lang, title(lang), &body)
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum StateOfInvoice {
    Ready,
    Held,
    Issued,
}

#[derive(Deserialize)]
struct InvoiceSummary {
    account: String,
    period: String,
    state: StateOfInvoice,
    review: Option<String>,
    number: Option<String>,
    currency: String,
    net: String,
    tax: String,
    gross: String,
    rounding: String,
    payable: String,
    lines: Vec<Line>,
    withheld: Vec<Withheld>,
    supersedes: Option<String>,
    credit: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum LineKind {
    Monthly,
    Session,
}

#[derive(Deserialize)]
struct Line {
    net: String,
    tax: String,
    kind: LineKind,
    agreement: String,
    source: String,
    offer: String,
    gross: String,
}

#[derive(Deserialize)]
struct Withheld {
    source: String,
    reason: String,
}

pub(super) fn button(
    action: &str,
    account: &str,
    period: &str,
    extra: Option<(&str, &str)>,
    label: &str,
    lang: Lang,
) -> String {
    let hidden = extra
        .map(|(name, value)| {
            format!(
                r#"<input type="hidden" name="{name}" value="{}">"#,
                escape(value)
            )
        })
        .unwrap_or_default();
    format!(
        r#"<form method="post" action="/retail-invoices/{action}?lang={tag}"><input type="hidden" name="account" value="{account}"><input type="hidden" name="period" value="{period}">{hidden}<button type="submit" class="btn">{label}</button></form>"#,
        tag = lang.tag(),
        account = escape(account),
        period = escape(period)
    )
}

fn reason(code: &str, lang: Lang) -> &'static str {
    match code {
        "payable_out_of_range" => lang.pick("Der Zahlbetrag liegt außerhalb des unterstützten Betragsbereichs.", "The payable amount exceeds the supported receivables range."),
        "line_credit_issued" => lang.pick("Diese Rechnung enthält bereits Positionsgutschriften. Prüfen Sie die betroffenen Positionen einzeln.", "This invoice already has line credits. Review its affected lines individually."),
        "withdrawal_credit_issued" => lang.pick("Eine Widerrufsgutschrift hat diese Rechnung bereits storniert.", "A withdrawal credit has already reversed this invoice."),
        "partial_month" => lang.pick("Für einen Teilmonat fehlt die vereinbarte Gebührenregel.", "The agreement has no fee rule for this partial month."),
        "no_billable_items" => lang.pick("Keine abrechenbaren Ladevorgänge oder Monatsbeiträge.", "No billable charging sessions or monthly fees."),
        "missing_authorization" | "authorization_mismatch" => lang.pick("Vertrag, Preis oder Freigabe passen nicht zum Ladevorgang.", "The session does not match its agreement, price or authorization."),
        "unsupported_tax_evidence" => lang.pick("Lieferant, Rechnungsempfänger oder Steuerangaben sind unvollständig oder werden nicht unterstützt.", "The supplier, recipient or tax details are incomplete or unsupported."),
        "unsupported_offer" | "fractional_monthly_fee" => lang.pick("Das Preisangebot lässt sich nicht eindeutig abrechnen.", "The accepted offer cannot be billed unambiguously."),
        "unverified_evidence" | "unrateable_evidence" => lang.pick("Die Messnachweise reichen für die Abrechnung nicht aus.", "The meter evidence is insufficient to bill this session."),
        "cdr_correction_requires_review" => lang.pick("Die Korrektur des Ladedatensatzes muss geprüft werden.", "The corrected charge detail record needs review."),
        "conflicting_agreement" => lang.pick("Die Vertragszuordnung ist widersprüchlich.", "The agreement assignment is inconsistent."),
        _ => lang.pick("Der Abrechnungsdienst kann diese Position nicht freigeben.", "The billing service could not validate this item."),
    }
}

fn evidence(body: &mut String, r: &InvoiceSummary, lang: Lang) {
    if !r.lines.is_empty() {
        let _ = write!(
            body,
            "<section><h2>{}</h2><ul>",
            lang.pick("Rechnungspositionen", "Invoice lines")
        );
        for line in &r.lines {
            let kind = match line.kind {
                LineKind::Monthly => lang.pick("Monatsbeitrag", "Monthly fee"),
                LineKind::Session => lang.pick("Ladevorgang", "Charging session"),
            };
            let _ = write!(
                body,
                "<li><strong>{kind} · {} {}</strong><details><summary>{}</summary><dl class=\"kv\"><dt>{net}</dt><dd>{net_amount} {cur}</dd><dt>{vat}</dt><dd>{tax_amount} {cur}</dd><dt>{}</dt><dd>{}</dd><dt>{}</dt><dd><code>{}</code></dd><dt>{}</dt><dd><code>{}</code></dd></dl></details></li>",
                escape(&line.gross),
                escape(&r.currency),
                lang.pick("Nachweise", "Evidence"),
                lang.pick("Quelle", "Source"),
                escape(&line.source),
                lang.pick("Vertrag", "Agreement"),
                escape(&line.agreement),
                lang.pick("Preisreferenz", "Offer reference"),
                escape(&line.offer),
                net = lang.pick("Netto", "Net"),
                vat = lang.pick("Umsatzsteuer", "VAT"),
                net_amount = escape(&line.net),
                tax_amount = escape(&line.tax),
                cur = escape(&r.currency),
            );
        }
        body.push_str("</ul></section>");
    }
    if !r.withheld.is_empty() {
        let _ = write!(
            body,
            "<section><h2>{}</h2><ul>",
            lang.pick("Zurückgestellte Positionen", "Withheld items")
        );
        for item in &r.withheld {
            let _ = write!(
                body,
                "<li>{} · {}</li>",
                escape(&item.source),
                reason(&item.reason, lang)
            );
        }
        body.push_str("</ul></section>");
    }
}

fn issued_controls(r: &InvoiceSummary, access: OperatorAccess, lang: Lang) -> Option<String> {
    let mut body = String::new();
    if access.can_write() {
        body.push_str(&crate::finance::adjustment::open_invoice(
            r.number.as_deref()?,
            r.lines.iter().map(|line| line.agreement.as_str()),
            lang,
        ));
    }
    for (number, label) in [
        (
            Some(r.number.as_deref()?),
            lang.pick("Rechnung laden (UBL)", "Download invoice (UBL)"),
        ),
        (
            r.credit.as_deref(),
            lang.pick("Stornobeleg laden (UBL)", "Download credit (UBL)"),
        ),
        (
            r.supersedes.as_deref(),
            lang.pick("Frühere Rechnung laden (UBL)", "Download original (UBL)"),
        ),
    ] {
        if let Some(n) = number {
            body.push_str(&button(
                "document",
                &r.account,
                &r.period,
                Some(("number", n)),
                label,
                lang,
            ));
        }
    }
    Some(body)
}

fn result(r: &InvoiceSummary, access: OperatorAccess, lang: Lang) -> Option<Response> {
    let heading = match r.state {
        StateOfInvoice::Ready => lang.pick("Abrechnung prüfen", "Review retail charges"),
        StateOfInvoice::Held => lang.pick("Abrechnung zurückgestellt", "Billing period on hold"),
        StateOfInvoice::Issued => lang.pick("Ausgestellte Kundenrechnung", "Issued retail invoice"),
    };
    let mut body = format!(
        "<p><strong>{}</strong> · {} (UTC)</p>",
        escape(&r.account),
        escape(&r.period)
    );
    for (label, number) in [
        (lang.pick("Rechnungsnummer", "Invoice number"), &r.number),
        (
            lang.pick("Ersetzt Rechnung", "Replaces invoice"),
            &r.supersedes,
        ),
        (lang.pick("Stornobeleg", "Credit document"), &r.credit),
    ] {
        if let Some(number) = number {
            let _ = write!(body, "<p>{label} <strong>{}</strong></p>", escape(number));
        }
    }
    evidence(&mut body, r, lang);
    body.push_str(r#"<dl class="kv">"#);
    for (label, value) in [
        (lang.pick("Netto", "Net"), &r.net),
        (lang.pick("Umsatzsteuer", "VAT"), &r.tax),
        (lang.pick("Brutto", "Gross"), &r.gross),
        (lang.pick("Rundung", "Rounding"), &r.rounding),
        (lang.pick("Zahlbetrag", "Payable"), &r.payable),
    ] {
        let _ = write!(
            body,
            "<dt>{label}</dt><dd>{} {}</dd>",
            escape(value),
            escape(&r.currency)
        );
    }
    body.push_str("</dl>");
    match r.state {
        StateOfInvoice::Ready if access.can_write() => {
            let review = r.review.as_deref().filter(|v| valid_reference(v))?;
            let label = if r.supersedes.is_some() {
                lang.pick(
                    "Stornieren und neu ausstellen",
                    "Issue credit and replacement",
                )
            } else {
                lang.pick("Rechnung ausstellen", "Issue invoice")
            };
            body.push_str(&button(
                "close",
                &r.account,
                &r.period,
                Some(("review", review)),
                label,
                lang,
            ));
        }
        StateOfInvoice::Ready => {
            let _ = write!(body, "<p>{}</p>", crate::read_only(lang));
        }
        StateOfInvoice::Issued if access.can_finance() => {
            body.push_str(&issued_controls(r, access, lang)?);
        }
        StateOfInvoice::Issued => body.push_str(lang.pick(
            "<p>Zum Laden der Originalrechnung ist ein Finanzzugriff erforderlich.</p>",
            "<p>A finance grant is required to download the original invoice.</p>",
        )),
        StateOfInvoice::Held => {}
    }
    body.push_str(&back(lang));
    Some(shell(lang, heading, &body))
}

fn problem(code: StatusCode, lang: Lang, message: &str, form: Option<&Request>) -> Response {
    let mut body = format!(r#"<p role="alert">{message}</p>"#);
    if let Some(f) = form {
        body.push_str(&button(
            "lookup",
            &f.account,
            &f.period,
            None,
            lang.pick("Ausstellung prüfen", "Check issued invoice"),
            lang,
        ));
    }
    body.push_str(&back(lang));
    with_status(shell(lang, title(lang), &body), code)
}

async fn submit(
    State(desk): State<Arc<Page>>,
    Extension(access): Extension<OperatorAccess>,
    Path(action): Path<String>,
    Query(params): Query<Params>,
    headers: HeaderMap,
    input: Result<Form<Request>, axum::extract::rejection::FormRejection>,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    if !matches!(action.as_str(), "review" | "close" | "lookup" | "document") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(token) = stations::operator_token(&headers).filter(|_| {
        stations::same_origin(&headers)
            && (action != "close" || access.can_write())
            && (action != "document" || access.can_finance())
    }) else {
        return problem(
            StatusCode::FORBIDDEN,
            lang,
            lang.pick(
                "Der Zugriff auf diesen Vorgang ist nicht erlaubt.",
                "Access to this operation is denied.",
            ),
            None,
        );
    };
    let Ok(Form(f)) = input else {
        return problem(
            StatusCode::BAD_REQUEST,
            lang,
            lang.pick(
                "Prüfen Sie Konto und Monat.",
                "Check the account and month.",
            ),
            None,
        );
    };
    let extra_valid = match action.as_str() {
        "close" => f.review.as_deref().is_some_and(valid_reference) && f.number.is_none(),
        "document" => {
            f.review.is_none()
                && f.number
                    .as_ref()
                    .is_some_and(|n| !n.is_empty() && n.len() <= 128)
        }
        _ => f.review.is_none() && f.number.is_none(),
    };
    if !valid_account(&f.account) || !valid_period(&f.period) || !extra_valid {
        return problem(
            StatusCode::BAD_REQUEST,
            lang,
            lang.pick(
                "Prüfen Sie die Kontoreferenz und den Abrechnungsmonat.",
                "Check the billing account reference and period.",
            ),
            None,
        );
    }
    let endpoint = format!(
        "{}/v1/billd/retail/{action}",
        desk.billd.trim_end_matches('/')
    );
    let payload = serde_json::json!(&f);
    let outcome = if action == "document" {
        desk.reader
            .document(&endpoint, &token, &payload)
            .await
            .map(|body| {
                (
                    [
                        (header::CONTENT_TYPE, "application/xml"),
                        (
                            header::CONTENT_DISPOSITION,
                            "attachment; filename=\"invoice.xml\"",
                        ),
                        (header::CACHE_CONTROL, "private, no-store"),
                    ],
                    body,
                )
                    .into_response()
            })
    } else {
        desk.reader
            .operator_action::<InvoiceSummary>(&endpoint, &token, &payload)
            .await
            .and_then(|r| {
                if r.account != f.account || r.period != f.period {
                    return Err(Why::Unreadable);
                }
                result(&r, access, lang).ok_or(Why::Unreadable)
            })
    };
    match outcome {
        Ok(response) => response,
        Err(error) => {
            let (code, message) = upstream_error(error, lang);
            problem(code, lang, message, (action == "close").then_some(&f))
        }
    }
}

fn upstream_error(error: Why, lang: Lang) -> (StatusCode, &'static str) {
    match error {
        Why::Status(404) => (
            StatusCode::NOT_FOUND,
            lang.pick(
                "Kein Vorgang für dieses Konto und diesen Monat gefunden.",
                "No matching record for this account and period.",
            ),
        ),
        Why::Refused => (
            StatusCode::FORBIDDEN,
            lang.pick(
                "Der Abrechnungsdienst verweigert den Zugriff.",
                "Billing denied access.",
            ),
        ),
        Why::Status(409 | 422 | 400) => (
            StatusCode::CONFLICT,
            lang.pick(
                "Prüfen Sie den Abrechnungsmonat und die Nachweise erneut.",
                "Review the billing month and evidence again.",
            ),
        ),
        _ => (
            StatusCode::BAD_GATEWAY,
            lang.pick(
                "Der Abrechnungsdienst hat kein bestätigtes Ergebnis geliefert.",
                "The billing service did not return a confirmed result.",
            ),
        ),
    }
}
