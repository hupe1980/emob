//! Native enrolment. Forms carry private inputs to the provider in POST bodies.
use core::fmt::Write as _;
use std::sync::Arc;

use axum::extract::rejection::FormRejection;
use axum::extract::{DefaultBodyLimit, Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Extension, Router};
use emob_core::{Emaid, EvseId, PartyId, TimeZone};
use emob_service::authority::OperatorAccess;
use emob_service::estate::RetailOffer;
use emob_service::ui::{Lang, escape};
use emob_tariff::Dimension;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;

use crate::fetch::Why;
use crate::investigation::{Params, moment, post_button, shell, valid_reference, with_status};
use crate::{Page, language, stations, writer};

pub(super) fn routes() -> Router<Arc<Page>> {
    Router::new()
        .route("/contracts", get(index))
        .route("/contracts/preview", post(preview))
        .route("/contracts/confirm", post(confirm))
        .route("/contracts/lookup", post(lookup))
        .layer(DefaultBodyLimit::max(2048))
}

#[derive(Deserialize)]
struct IndexParams {
    lang: Option<String>,
    #[serde(default)]
    page: usize,
}

#[derive(Deserialize)]
struct Options {
    offers: Vec<OptionView>,
}

#[derive(Deserialize)]
struct OptionView {
    point: EvseId,
    provider: PartyId,
    plan: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Intake {
    selection: String,
    stock: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    operation: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    agreement: String,
}

#[derive(Deserialize)]
struct Review {
    card: String,
    operation: String,
    offer: RetailOffer,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

#[derive(Deserialize)]
struct Receipt {
    agreement: String,
    emaid: Emaid,
    card: String,
    reference: String,
    actor: String,
    offer: RetailOffer,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
}

fn endpoint(desk: &Page, action: &str) -> String {
    format!(
        "{}/v1/empd/contracts/{action}",
        desk.empd.trim_end_matches('/')
    )
}

fn back(lang: Lang) -> String {
    format!(
        "<p class=\"back noprint\"><a href=\"/contracts?lang={}\">{}</a></p>",
        lang.tag(),
        lang.pick("Zur Vertragsübersicht", "Back to driver contracts")
    )
}

fn failure(code: StatusCode, lang: Lang, message: &str) -> Response {
    with_status(
        shell(
            lang,
            lang.pick("Ladeverträge", "Driver contracts"),
            &format!("<p role=\"alert\">{message}</p>{}", back(lang)),
        ),
        code,
    )
}

fn access_error(lang: Lang) -> Response {
    failure(
        StatusCode::FORBIDDEN,
        lang,
        lang.pick(
            "Öffnen Sie die Vertragsübersicht mit einem Schreibzugang auf dieser Seite.",
            "Open driver contracts on this site with an account that can enrol a card.",
        ),
    )
}

fn invalid(lang: Lang, code: StatusCode) -> Response {
    failure(
        code,
        lang,
        lang.pick(
            "Prüfen Sie die Eingabe und öffnen Sie das Formular erneut.",
            "Check the details and reopen the form.",
        ),
    )
}

fn masked(label: &str) -> bool {
    label.strip_prefix("EM-").is_some_and(|s| {
        s.len() == 12
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'A'..=b'F').contains(&c))
    })
}

fn receipt_lookup(lang: Lang) -> String {
    format!(
        "<section class=\"card noprint\"><h2>{heading}</h2><form method=\"post\" action=\"/contracts/lookup?lang={tag}\" autocomplete=\"off\"><p><label>{reference}<br><input name=\"agreement\" type=\"text\" required minlength=\"64\" maxlength=\"64\" pattern=\"[0-9a-f]{{64}}\" spellcheck=\"false\"></label></p><button type=\"submit\" class=\"btn ghost\">{open}</button></form></section>",
        tag = lang.tag(),
        heading = lang.pick("Vertrag nachschlagen", "Find an agreement"),
        reference = lang.pick("Vertragsreferenz", "Agreement reference"),
        open = lang.pick("Vertrag öffnen", "Open agreement")
    )
}

fn enrol_form(offers: &[OptionView], requested_page: usize, lang: Lang) -> String {
    const PAGE: usize = 24;
    let last = offers.len().saturating_sub(1) / PAGE;
    let page = requested_page.min(last);
    let mut options = String::new();
    for item in offers.iter().skip(page * PAGE).take(PAGE) {
        let _ = write!(
            options,
            "<option value=\"{point}|{plan}\">{name} · {provider} · {point} ({plan})</option>",
            point = item.point,
            provider = item.provider,
            name = escape(&item.name),
            plan = escape(&item.plan)
        );
    }
    let mut links = String::new();
    for (index, label) in [
        (
            page.checked_sub(1),
            lang.pick("Vorherige Angebote", "Previous offers"),
        ),
        (
            (page < last).then_some(page + 1),
            lang.pick("Weitere Angebote", "Next offers"),
        ),
    ] {
        if let Some(index) = index {
            let _ = write!(
                links,
                "<a class=\"btn ghost\" href=\"/contracts?lang={}&amp;page={index}\">{label}</a>",
                lang.tag()
            );
        }
    }
    let content = if options.is_empty() {
        format!(
            "<p>{}</p>",
            lang.pick(
                "Der Anbieter hat kein Angebot für die Anmeldung freigegeben.",
                "The provider has no offers available for enrolment."
            )
        )
    } else {
        format!(
            "<form method=\"post\" action=\"/contracts/preview?lang={tag}\" autocomplete=\"off\"><p><label>{offer}<br><select name=\"selection\" required><option value=\"\">{choose}</option>{options}</select></label></p><p><label>{card}<br><input name=\"stock\" type=\"text\" required maxlength=\"32\" pattern=\"[A-Z0-9][A-Z0-9\\-]{{0,31}}\" spellcheck=\"false\" aria-describedby=\"stock-help\"></label><br><small id=\"stock-help\">{help}</small></p><button type=\"submit\" class=\"btn\">{review}</button></form><div class=\"card-controls\">{links}</div>",
            tag = lang.tag(),
            offer = lang.pick("Tarif und Ladepunkt", "Plan and charging point"),
            choose = lang.pick("Angebot auswählen", "Choose an offer"),
            card = lang.pick("Bestandsnummer der Karte", "Card stock number"),
            help = lang.pick(
                "Die Nummer auf der noch nicht zugeteilten Karte besteht aus Großbuchstaben, Ziffern und Bindestrichen.",
                "Enter the inventory code on the unissued card (uppercase letters, digits and hyphens)."
            ),
            review = lang.pick("Vertrag prüfen", "Review contract")
        )
    };
    format!(
        "<section class=\"card noprint\"><h2>{}</h2>{content}</section>",
        lang.pick("Karte für einen Vertrag anmelden", "Enrol a card")
    )
}

async fn index(
    State(desk): State<Arc<Page>>,
    Extension(access): Extension<OperatorAccess>,
    Query(params): Query<IndexParams>,
    headers: HeaderMap,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    let (code, mut body) = if !access.can_write() {
        (StatusCode::OK, format!("<p>{}</p>", crate::read_only(lang)))
    } else if let Ok(options) = desk
        .reader
        .fetch::<Options>(&endpoint(&desk, "options"))
        .await
    {
        (
            StatusCode::OK,
            enrol_form(&options.offers, params.page, lang),
        )
    } else {
        let message = lang.pick(
            "Die Angebote des Anbieters sind derzeit nicht verfügbar. Versuchen Sie es später erneut.",
            "Provider offers are temporarily unavailable. Try again later.",
        );
        (
            StatusCode::BAD_GATEWAY,
            format!("<p role=\"alert\">{message}</p>"),
        )
    };
    body.push_str(&receipt_lookup(lang));
    with_status(
        shell(lang, lang.pick("Ladeverträge", "Driver contracts"), &body),
        code,
    )
}

fn action_form(action: &str, name: &str, value: &str, label: &str, lang: Lang) -> String {
    format!(
        "<form method=\"post\" class=\"noprint\" action=\"/contracts/{action}?lang={}\"><input type=\"hidden\" name=\"{name}\" value=\"{}\"><button type=\"submit\" class=\"btn\">{label}</button></form>",
        lang.tag(),
        escape(value)
    )
}

fn terms(offer: &RetailOffer, lang: Lang, zone: &TimeZone) -> Option<String> {
    if offer.terms.reference() != offer.reference {
        return None;
    }
    let t = &offer.terms;
    let tariff = t.retail_tariff().ok()?;
    let prices = emob_tariff::describe_gross(&tariff, t.from()).ok()?;
    let currency = t.fees.currency;
    let amount = |value: Decimal| {
        let mut value = value.normalize();
        value.rescale(value.scale().max(2));
        lang.decimal(value)
    };
    let mut rows = String::new();
    for line in &prices.lines {
        let (dimension, unit) = match line.dimension {
            Dimension::Energy => (lang.pick("Energie", "Energy"), "kWh"),
            Dimension::Flat => (
                lang.pick("Sitzungsgebühr", "Session fee"),
                lang.pick("Sitzung", "session"),
            ),
            Dimension::Time => (lang.pick("Ladezeit", "Charging time"), "min"),
            Dimension::ParkingTime => (lang.pick("Belegung", "Occupancy"), "min"),
        };
        let _ = write!(
            rows,
            "<dt>{dimension}</dt><dd>{} {currency}/{unit}</dd>",
            amount(line.price)
        );
    }
    let until = t.until().map_or_else(
        || lang.pick("auf Weiteres", "Until further notice").to_owned(),
        |at| moment(at, zone, lang),
    );
    Some(format!(
        "<section class=\"card\"><h2>{heading}</h2><p><strong>{name}</strong> · {provider} · {plan}</p><dl class=\"kv\"><dt>{point}</dt><dd>{evse}</dd>{rows}<dt>{monthly}</dt><dd>{amount} {currency}/{month}</dd><dt>{vat}</dt><dd>{rate}%</dd><dt>{from}</dt><dd>{start}</dd><dt>{to}</dt><dd>{until}</dd><dt>{version}</dt><dd>{v}</dd><dt>{digest}</dt><dd><code>{id}</code></dd></dl><details><summary>{breakdown}</summary><dl class=\"kv\"><dt>{energy_fee}</dt><dd>{energy} {currency}/kWh</dd><dt>{flat_fee}</dt><dd>{flat} {currency}/{session}</dd><dt>E-Roaming</dt><dd>{roaming} {currency}/{session}</dd></dl></details></section>",
        heading = lang.pick("Vereinbarte Bruttopreise", "Gross contract prices"),
        name = escape(&t.name),
        provider = t.provider,
        plan = escape(&t.code),
        point = lang.pick("Ladepunkt", "Charging point"),
        evse = t.point,
        monthly = lang.pick("Monatsbeitrag", "Monthly subscription"),
        month = lang.pick("Monat", "month"),
        amount = amount(t.fees.monthly),
        vat = lang.pick("Enthaltene MwSt.", "VAT included"),
        rate = lang.decimal(t.fees.vat.normalize()),
        from = lang.pick("Preis gültig ab", "Price valid from"),
        start = moment(t.from(), zone, lang),
        to = lang.pick("Preis gültig bis", "Price valid until"),
        version = lang.pick("Anbieterpreisstand", "Provider fee version"),
        v = escape(&t.fees.version),
        digest = lang.pick("Preisreferenz", "Offer reference"),
        id = escape(&offer.reference),
        breakdown = lang.pick("Enthaltene Anbietergebühren", "Included provider charges"),
        energy_fee = lang.pick("Energieaufschlag", "Energy surcharge"),
        flat_fee = lang.pick("Sitzungsaufschlag", "Session surcharge"),
        energy = amount(t.fees.energy),
        flat = amount(t.fees.session),
        roaming = amount(t.fees.roaming),
        session = lang.pick("Sitzung", "session"),
    ))
}

fn review_view(review: &Review, lang: Lang, zone: &TimeZone) -> Option<Response> {
    let price = terms(&review.offer, lang, zone)?;
    let body = format!("<section class=\"card\"><h2>{heading}</h2><dl class=\"kv\"><dt>{card}</dt><dd>{masked}</dd><dt>{by}</dt><dd>{expiry}</dd></dl><p>{effect}</p></section>{price}<div class=\"card-controls\">{confirm}{back}</div>",
        heading=lang.pick("Karte anmelden","Enrol card"),card=lang.pick("Kartennummer des Anbieters","Provider card number"),masked=escape(&review.card),by=lang.pick("Bestätigen bis","Confirm by"),expiry=moment(review.expires_at,zone,lang),
        effect=lang.pick("Mit der Bestätigung teilt der Anbieter die Karte für diesen Ladepunkt und genau diesen Preisstand zu.","Confirmation issues the card for online charging at this point under these exact terms."),
        confirm=action_form("confirm","operation",&review.operation,lang.pick("Vertrag anlegen und Karte zuteilen","Confirm enrolment"),lang),back=back(lang));
    Some(shell(
        lang,
        lang.pick("Ladevertrag prüfen", "Review driver contract"),
        &body,
    ))
}

fn error_page(error: Why, lang: Lang, action: &str) -> Response {
    match error {
        Why::Refused => access_error(lang),
        Why::Status(404) if action == "lookup" => failure(StatusCode::NOT_FOUND,lang,lang.pick("Zu dieser Referenz wurde kein Vertrag gefunden.","No agreement was found for this reference.")),
        Why::Status(404) => failure(StatusCode::NOT_FOUND,lang,lang.pick("Die Bestandsnummer oder das gewählte Angebot ist unbekannt. Prüfen Sie beides.","The stock number or selected offer could not be found. Check both details.")),
        Why::Status(409) => failure(StatusCode::CONFLICT,lang,lang.pick("Die Vorschau ist nicht mehr gültig oder die Karte wurde bereits zugeteilt. Prüfen Sie die Vertragsreferenz und fordern Sie bei Bedarf eine neue Vorschau an.","The review is no longer current or the card has already been issued. Check the agreement reference before starting another review.")),
        Why::Status(400 | 422) => invalid(lang, StatusCode::BAD_REQUEST),
        _ => failure(StatusCode::BAD_GATEWAY,lang,lang.pick("Der Anbieter ist derzeit nicht verfügbar. Versuchen Sie es später erneut.","The provider is currently unavailable. Try again later.")),
    }
}

async fn preview(
    State(desk): State<Arc<Page>>,
    Extension(access): Extension<OperatorAccess>,
    Query(params): Query<Params>,
    headers: HeaderMap,
    input: Result<Form<Intake>, FormRejection>,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    let Ok(token) = writer(&headers, access) else {
        return access_error(lang);
    };
    let Form(input) = match input {
        Ok(form) => form,
        Err(error) => return invalid(lang, error.status()),
    };
    let Some((point, plan)) = input.selection.split_once('|') else {
        return invalid(lang, StatusCode::BAD_REQUEST);
    };
    if input.stock.is_empty()
        || input.stock.len() > 32
        || plan.is_empty()
        || plan.len() > 40
        || point.parse::<EvseId>().is_err()
    {
        return invalid(lang, StatusCode::BAD_REQUEST);
    }
    let payload = json!({"point":point,"plan":plan,"stock":input.stock});
    match desk
        .reader
        .operator_action::<Review>(&endpoint(&desk, "preview"), &token, &payload)
        .await
    {
        Ok(r)
            if valid_reference(&r.operation)
                && masked(&r.card)
                && valid_reference(&r.offer.reference)
                && r.offer.terms.point.to_string() == point
                && r.offer.terms.code == plan =>
        {
            review_view(&r, lang, &desk.zone)
                .unwrap_or_else(|| error_page(Why::Unreadable, lang, "preview"))
        }
        Ok(_) => error_page(Why::Unreadable, lang, "preview"),
        Err(e) => error_page(e, lang, "preview"),
    }
}

fn receipt_view(
    receipt: &Receipt,
    expected: &str,
    lang: Lang,
    zone: &TimeZone,
) -> Option<Response> {
    if receipt.agreement != expected
        || !valid_reference(&receipt.reference)
        || !masked(&receipt.card)
        || !receipt.reference[..12].eq_ignore_ascii_case(&receipt.card[3..])
        || !valid_reference(&receipt.offer.reference)
    {
        return None;
    }
    let pricing = terms(&receipt.offer, lang, zone)?;
    let body = format!(
        "<section class=\"card\"><h2>{issued}</h2><dl class=\"kv\"><dt>{reference}</dt><dd><code>{agreement}</code></dd><dt>eMAID</dt><dd>{emaid}</dd><dt>{card}</dt><dd>{masked}</dd><dt>{operator}</dt><dd>{actor}</dd><dt>{at_label}</dt><dd>{at}</dd></dl></section>{pricing}<div class=\"card-controls\">{history}{back}</div>",
        issued = lang.pick("Vertrag angelegt", "Agreement issued"),
        reference = lang.pick("Vertragsreferenz", "Agreement reference"),
        agreement = escape(&receipt.agreement),
        emaid = receipt.emaid,
        card = lang.pick("Kartennummer des Anbieters", "Provider card number"),
        masked = escape(&receipt.card),
        operator = lang.pick("Angelegt durch", "Issued by"),
        actor = escape(&receipt.actor),
        at_label = lang.pick("Zeitpunkt", "Issued at"),
        at = moment(receipt.at, zone, lang),
        history = post_button(
            "reference",
            &receipt.reference,
            None,
            lang.pick("Kartenstatus öffnen", "Open card status"),
            lang
        ),
        back = back(lang)
    );
    let detail = lang.pick(
        "Dieser Nachweis dokumentiert die Kartenzuteilung und den vereinbarten Preisstand. Die monatliche Fahrerabrechnung ist noch in Entwicklung.",
        "This record documents card fulfilment and the agreed terms. Monthly driver invoicing is planned separately.",
    );
    Some(shell(
        lang,
        lang.pick("Ladevertrag", "Driver contract"),
        &format!("{body}<p>{detail}</p>"),
    ))
}

async fn lookup(
    State(desk): State<Arc<Page>>,
    Query(params): Query<Params>,
    headers: HeaderMap,
    input: Result<Form<Lookup>, FormRejection>,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    if !stations::same_origin(&headers) {
        return failure(
            StatusCode::FORBIDDEN,
            lang,
            lang.pick(
                "Öffnen Sie die Vertragssuche auf dieser Seite.",
                "Start the agreement search on this site.",
            ),
        );
    }
    let Form(query) = match input {
        Ok(form) => form,
        Err(e) => return invalid(lang, e.status()),
    };
    if !valid_reference(&query.agreement) {
        return invalid(lang, StatusCode::BAD_REQUEST);
    }
    match desk
        .reader
        .lookup::<Receipt>(
            &endpoint(&desk, "lookup"),
            &json!({"agreement":query.agreement}),
        )
        .await
    {
        Ok(r) => receipt_view(&r, &query.agreement, lang, &desk.zone)
            .unwrap_or_else(|| error_page(Why::Unreadable, lang, "lookup")),
        Err(e) => error_page(e, lang, "lookup"),
    }
}

fn unconfirmed(id: &str, lang: Lang) -> Response {
    let body=format!("<p role=\"alert\">{message}</p><p>{reference} <code>{id}</code></p><div class=\"card-controls\">{check}{retry}</div>{back}",
        message=lang.pick("Die Bestätigung des Anbieters fehlt. Der Vertrag kann bereits bestehen. Prüfen Sie zuerst die Vertragsreferenz.","The provider’s response is unavailable. The agreement may already exist. Look up its reference first."),
        reference=lang.pick("Vertragsreferenz","Agreement reference"),id=escape(id),
        check=action_form("lookup","agreement",id,lang.pick("Vertrag suchen","Check agreement"),lang),
        retry=action_form("confirm","operation",id,lang.pick("Dieselbe Bestätigung wiederholen","Retry this confirmation"),lang),back=back(lang));
    with_status(
        shell(
            lang,
            lang.pick("Vertragsstatus prüfen", "Check agreement status"),
            &body,
        ),
        StatusCode::BAD_GATEWAY,
    )
}

async fn confirm(
    State(desk): State<Arc<Page>>,
    Extension(access): Extension<OperatorAccess>,
    Query(params): Query<Params>,
    headers: HeaderMap,
    input: Result<Form<Confirmation>, FormRejection>,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    let Ok(token) = writer(&headers, access) else {
        return access_error(lang);
    };
    let Form(input) = match input {
        Ok(form) => form,
        Err(e) => return invalid(lang, e.status()),
    };
    if !valid_reference(&input.operation) {
        return invalid(lang, StatusCode::BAD_REQUEST);
    }
    match desk
        .reader
        .operator_action::<Receipt>(
            &endpoint(&desk, "confirm"),
            &token,
            &json!({"operation":input.operation}),
        )
        .await
    {
        Ok(r) => receipt_view(&r, &input.operation, lang, &desk.zone)
            .unwrap_or_else(|| unconfirmed(&input.operation, lang)),
        Err(Why::Status(404 | 409)) => {
            let message = lang.pick(
                "Diese Vorschau ist nicht mehr gültig. Prüfen Sie, ob ein Vertrag vorliegt, oder wählen Sie ein aktuelles Angebot.",
                "This review is no longer current. Check for an existing agreement or review a current offer.",
            );
            let check = action_form(
                "lookup",
                "agreement",
                &input.operation,
                lang.pick("Vertrag suchen", "Check agreement"),
                lang,
            );
            with_status(
                shell(
                    lang,
                    lang.pick("Vertragsstatus prüfen", "Check agreement status"),
                    &format!("<p role=\"alert\">{message}</p>{check}{}", back(lang)),
                ),
                StatusCode::CONFLICT,
            )
        }
        Err(e @ (Why::Refused | Why::Status(400 | 422))) => error_page(e, lang, "confirm"),
        Err(_) => unconfirmed(&input.operation, lang),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_pages_keep_every_selection_reachable_in_both_languages() {
        let offers: Vec<_> = (1..=256)
            .map(|number| OptionView {
                point: "DE*ABC*E00001".parse().unwrap(),
                provider: "DE*MSP".parse().unwrap(),
                plan: format!("plan-{number}"),
                name: "<&>".repeat(53),
            })
            .collect();
        for (lang, next, previous) in [
            (Lang::De, "Weitere Angebote", "Vorherige Angebote"),
            (Lang::En, "Next offers", "Previous offers"),
        ] {
            let mut choices = 0;
            for page in 0..11 {
                let form = enrol_form(&offers, page, lang);
                choices += form.matches("DE*ABC*E00001|plan-").count();
                assert!(
                    form.contains("&lt;&amp;&gt;") && !form.contains("<&>") && form.len() < 150_000
                );
                assert_eq!(form.contains(next), page < 10);
                assert_eq!(form.contains(previous), page > 0);
            }
            assert_eq!(choices, 256);
            assert!(enrol_form(&offers, usize::MAX, lang).contains("plan-256"));
            assert!(!enrol_form(&[], 0, lang).contains("/contracts/preview"));
        }
    }
}
