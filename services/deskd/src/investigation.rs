//! An operator follows one immutable CSMS outcome or asks its owner for an exact match.
use core::fmt::Write as _;
use std::sync::Arc;

use axum::Extension;
use axum::extract::rejection::{FormRejection, PathRejection};
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use emob_core::TimeZone;
use emob_eichrecht::Finding;
use emob_service::authority::OperatorAccess;
use emob_service::ui::{self, Lang, escape};
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;

use crate::{
    Page,
    card::{self, Cards},
    fetch::Why,
    language, model, station, stations, why,
};

#[derive(Deserialize)]
pub(super) struct Params {
    pub(super) lang: Option<String>,
}

#[derive(Deserialize)]
struct Evidence {
    position: u64,
    identity: String,
    transaction: String,
    station_registered: bool,
    billed: bool,
    reasons: Vec<String>,
    #[serde(default)]
    omitted_findings: usize,
    #[serde(default)]
    omitted_reasons: usize,
    findings: Vec<Finding>,
    #[serde(with = "time::serde::rfc3339::option")]
    started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    ended_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    decided_at: OffsetDateTime,
    observed_frames: usize,
    missing_frames: usize,
    card_reference: Option<String>,
    receipt: Option<String>,
}

pub(super) fn shell(lang: Lang, title: &str, body: &str) -> Response {
    ui::html(crate::navigation::page(
        &ui::Shell {
            title,
            place: lang.pick("Betrieb", "Operations"),
            lang,
            reading: true,
            script: None,
        },
        &format!("<div class=\"investigation\"><h1>{title}</h1>{body}</div>"),
    ))
}

pub(super) fn with_status(mut response: Response, code: StatusCode) -> Response {
    *response.status_mut() = code;
    response
}

pub(super) fn moment(at: OffsetDateTime, zone: &TimeZone, lang: Lang) -> String {
    let local = zone.local(at);
    format!(
        "<time datetime=\"{}\">{} · {:02}:{:02}:{:02}</time>",
        at.format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        lang.date(local.date),
        local.time.hour(),
        local.time.minute(),
        local.time.second()
    )
}

fn session_title(lang: Lang) -> &'static str {
    lang.pick("Vorgangsnachweis", "Session evidence")
}

pub(super) async fn session(
    State(desk): State<Arc<Page>>,
    path: Result<Path<u64>, PathRejection>,
    Query(params): Query<Params>,
    headers: HeaderMap,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    let Ok(Path(position)) = path else {
        return with_status(
            shell(
                lang,
                session_title(lang),
                lang.pick(
                    "<p>Ungültige Vorgangsposition.</p>",
                    "<p>Invalid session position.</p>",
                ),
            ),
            StatusCode::BAD_REQUEST,
        );
    };
    let url = format!(
        "{}/v1/csmsd/sessions/{position}",
        desk.csmsd.trim_end_matches('/')
    );
    match desk.reader.fetch::<Evidence>(&url).await {
        Ok(facts) if facts.position == position => render(&facts, &desk.zone, &desk.receipts, lang),
        Err(Why::Status(404)) => with_status(
            shell(
                lang,
                session_title(lang),
                lang.pick(
                    "<p>Kein Vorgang unter dieser Nummer.</p>",
                    "<p>No session at this position.</p>",
                ),
            ),
            StatusCode::NOT_FOUND,
        ),
        _ => unavailable(lang, "csmsd"),
    }
}

fn render(facts: &Evidence, zone: &TimeZone, receipts: &str, lang: Lang) -> Response {
    let identity = escape(&facts.identity);
    let station = if facts.station_registered {
        format!(
            "<a href=\"{}?lang={}\">{identity}</a>",
            station::href(&facts.identity),
            lang.tag()
        )
    } else {
        format!(
            "{identity} · {}",
            lang.pick("nicht mehr im Stationsregister", "no longer registered")
        )
    };
    let state = if facts.billed {
        lang.pick("Abgerechnet", "Billed")
    } else {
        lang.pick("Nicht abgerechnet", "Not billed")
    };
    let mut body = format!(
        "<section class=\"card\"><h2>{state}</h2><dl class=\"kv\"><dt>Station</dt><dd class=\"back\">{station}</dd><dt>{session}</dt><dd>{tx}</dd></dl>",
        session = lang.pick("Vorgang", "Session"),
        tx = escape(&facts.transaction)
    );
    if !facts.billed {
        let (reason, _) = why::headline(lang, &facts.findings, &facts.reasons);
        let _ = write!(
            body,
            "<p>{reason}</p>{}",
            why::details(lang, &facts.findings, &facts.reasons)
        );
        body.push_str(&why::omitted(
            lang,
            facts.omitted_findings,
            facts.omitted_reasons,
        ));
    }
    body.push_str("</section>");
    body.push_str(&timeline(facts, zone, lang));
    body.push_str(&links(facts, receipts, lang));
    shell(lang, session_title(lang), &body)
}

fn timeline(facts: &Evidence, zone: &TimeZone, lang: Lang) -> String {
    let mut body = String::new();
    let _ = write!(
        body,
        "<section class=\"card\"><h2>{}</h2><ol class=\"timeline\">",
        lang.pick("Zeitlicher Ablauf", "Timeline")
    );
    for (at, event, source) in [
        (
            facts.started_at,
            lang.pick("Beginn", "Start"),
            lang.pick("Stationsuhr", "station clock"),
        ),
        (
            facts.ended_at,
            lang.pick("Ende", "End"),
            lang.pick("Stationsuhr", "station clock"),
        ),
        (
            Some(facts.decided_at),
            lang.pick("Abrechnungsentscheidung", "Billing decision"),
            "csmsd",
        ),
    ] {
        if let Some(at) = at {
            let _ = write!(
                body,
                "<li><strong>{event}</strong> {}<span class=\"muted\">{source}</span></li>",
                moment(at, zone, lang)
            );
        }
    }
    body.push_str("</ol>");
    let _ = write!(
        body,
        "<p>{}: <strong>{}</strong> · {}: <strong>{}</strong></p>",
        lang.pick("Beobachtete Nachrichten", "Observed frames"),
        facts.observed_frames,
        lang.pick("Fehlende Nachrichten", "Missing frames"),
        facts.missing_frames
    );
    if facts.started_at.is_none()
        || facts.ended_at.is_none()
        || facts.observed_frames == 0
        || facts.missing_frames > 0
    {
        let _ = write!(body, "<p>{}</p>",lang.pick("Der gespeicherte Ablauf ist unvollständig. Für fehlende Nachrichten liegt keine Beobachtung vor.", "The recorded timeline is incomplete. There is no observation for a missing frame."));
    }
    body.push_str("</section>");
    body
}

fn links(facts: &Evidence, receipts: &str, lang: Lang) -> String {
    let mut body = String::new();
    let _ = write!(
        body,
        "<section class=\"card\"><h2>{}</h2>",
        lang.pick("Karte und Beleg", "Card and receipt")
    );
    if let Some(reference) = facts
        .card_reference
        .as_deref()
        .filter(|r| valid_reference(r))
    {
        let label = format!(
            "{} …{}",
            lang.pick("Karte öffnen", "Open card"),
            &reference[reference.len() - 6..]
        );
        body.push_str(&post_button("reference", reference, None, &label, lang));
    } else {
        let _ = write!(
            body,
            "<p>{}</p>",
            lang.pick(
                "Keine zugeordnete Kartenreferenz.",
                "No linked card reference."
            )
        );
    }
    if let Some(href) = facts
        .receipt
        .as_deref()
        .and_then(|path| receipt_url(receipts, path))
    {
        let _ = write!(
            body,
            "<p class=\"back\"><a href=\"{}\">{}</a></p>",
            escape(&href),
            lang.pick("Beleg öffnen", "Open receipt")
        );
    } else {
        let _ = write!(
            body,
            "<p>{}</p>",
            lang.pick("Kein Beleg verfügbar.", "No receipt available.")
        );
    }
    body.push_str("</section>");
    body
}

/// Only the owner's receipt capability shape can leave this page.
pub(crate) fn receipt_url(base: &str, path: &str) -> Option<String> {
    let token = path.strip_prefix("/r/")?;
    (!base.is_empty()
        && token.len() == 22
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)))
    .then(|| format!("{}{path}", base.trim_end_matches('/')))
}

pub(super) fn valid_reference(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Deserialize)]
pub(super) struct Input {
    kind: String,
    value: String,
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct Match {
    position: u64,
    identity: String,
    transaction: String,
    #[serde(with = "time::serde::rfc3339")]
    decided_at: OffsetDateTime,
}

pub(super) async fn lookup(Query(params): Query<Params>, headers: HeaderMap) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    lookup_page(lang, "")
}

fn lookup_page(lang: Lang, results: &str) -> Response {
    let title = lang.pick("Vorgang oder Karte suchen", "Find a session or card");
    let form = format!(
        "<form method=\"post\" action=\"/lookup?lang={tag}\" autocomplete=\"off\" class=\"card noprint\"><p><label>{kind}<select name=\"kind\"><option value=\"transaction\">{session}</option><option value=\"visual\">{visual}</option><option value=\"emaid\">eMAID</option></select></label></p><p><label>{identifier}<input type=\"text\" name=\"value\" required maxlength=\"256\"></label></p><button class=\"btn\" type=\"submit\">{search}</button></form>",
        tag = lang.tag(),
        kind = lang.pick("Suchfeld", "Search by"),
        session = lang.pick("Lokale Vorgangsnummer", "Local transaction reference"),
        visual = lang.pick(
            "Kartennummer oder emob-Suchnummer",
            "Printed card number or emob lookup number"
        ),
        identifier = lang.pick("Kennung", "Identifier"),
        search = lang.pick("Suchen", "Search")
    );
    shell(lang, title, &format!("{results}{form}"))
}

pub(super) fn post_button(
    kind: &str,
    value: &str,
    cursor: Option<&str>,
    label: &str,
    lang: Lang,
) -> String {
    let field = cursor.map_or_else(String::new, |c| {
        format!(
            "<input type=\"hidden\" name=\"cursor\" value=\"{}\">",
            escape(c)
        )
    });
    format!(
        "<form class=\"noprint\" method=\"post\" action=\"/lookup?lang={}\" autocomplete=\"off\"><input type=\"hidden\" name=\"kind\" value=\"{}\"><input type=\"hidden\" name=\"value\" value=\"{}\">{field}<button class=\"btn ghost\" type=\"submit\">{}</button></form>",
        lang.tag(),
        escape(kind),
        escape(value),
        escape(label)
    )
}

fn invalid(lang: Lang) -> Response {
    with_status(
        lookup_page(
            lang,
            lang.pick(
                "<p role=\"alert\">Kennung oder Seitenposition ungültig.</p>",
                "<p role=\"alert\">Invalid identifier or page position.</p>",
            ),
        ),
        StatusCode::BAD_REQUEST,
    )
}

pub(super) fn unavailable(lang: Lang, owner: &str) -> Response {
    let message = format!(
        "<p>{owner} · {}</p>",
        lang.pick(
            "Datenquelle nicht verfügbar. Bitte erneut versuchen.",
            "Data source unavailable. Please try again."
        )
    );
    with_status(
        shell(
            lang,
            lang.pick("Daten nicht verfügbar", "Data unavailable"),
            &message,
        ),
        StatusCode::BAD_GATEWAY,
    )
}

pub(super) async fn search(
    State(desk): State<Arc<Page>>,
    Extension(access): Extension<OperatorAccess>,
    Query(params): Query<Params>,
    headers: HeaderMap,
    form: Result<Form<Input>, FormRejection>,
) -> Response {
    let lang = language(params.lang.as_deref(), &headers);
    if !stations::same_origin(&headers) {
        return with_status(
            lookup_page(
                lang,
                lang.pick(
                    "<p>Suche auf dieser Seite starten.</p>",
                    "<p>Start the search on this page.</p>",
                ),
            ),
            StatusCode::FORBIDDEN,
        );
    }
    let input = match form {
        Ok(Form(input)) => input,
        Err(rejection) => return with_status(invalid(lang), rejection.status()),
    };
    let value = input.value.trim();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return invalid(lang);
    }
    match input.kind.as_str() {
        "transaction" => session_matches(&desk, &input, lang).await,
        "reference" | "visual" | "emaid" => {
            card_matches(&desk, &input, lang, access.can_write()).await
        }
        _ => invalid(lang),
    }
}

async fn session_matches(desk: &Page, input: &Input, lang: Lang) -> Response {
    let value = input.value.trim();
    let mut list = String::new();
    let Ok(before) = input.cursor.as_deref().map(str::parse::<u64>).transpose() else {
        return invalid(lang);
    };
    let url = format!(
        "{}/v1/csmsd/sessions/search",
        desk.csmsd.trim_end_matches('/')
    );
    let hits: model::Page<Match> = match desk
        .reader
        .lookup(
            &url,
            &json!({"transaction":value,"before":before,"limit":20}),
        )
        .await
    {
        Ok(page) => page,
        Err(Why::Status(400 | 422)) => return invalid(lang),
        Err(_) => return unavailable(lang, "csmsd"),
    };
    for row in &hits.items {
        let _ = write!(
            list,
            "<li><a href=\"/sessions/{}?lang={}\">{} · {}</a> · {}</li>",
            row.position,
            lang.tag(),
            escape(&row.identity),
            escape(&row.transaction),
            moment(row.decided_at, &desk.zone, lang)
        );
    }
    matches_page(
        input,
        lang,
        &list,
        hits.older.map(|n| n.to_string()).as_deref(),
    )
}

async fn card_matches(desk: &Page, input: &Input, lang: Lang, can_write: bool) -> Response {
    let value = input.value.trim();
    let mut list = String::new();
    if input.cursor.as_deref().is_some_and(|c| !valid_reference(c)) {
        return invalid(lang);
    }
    let url = format!("{}/v1/empd/cards/lookup", desk.empd.trim_end_matches('/'));
    let cards: Cards = match desk
        .reader
        .lookup(
            &url,
            &json!({"kind":input.kind,"value":value,"after":input.cursor}),
        )
        .await
    {
        Ok(page) => page,
        Err(Why::Status(400 | 422)) => return invalid(lang),
        Err(Why::Refused) => {
            return with_status(
                lookup_page(lang, lang.pick(
                    "<p role=\"alert\">Der Anbieter verweigert dem Lesezugang Zugriff auf die Kartendaten.</p>",
                    "<p role=\"alert\">The provider denied the desk read access to its card records.</p>",
                )),
                StatusCode::BAD_GATEWAY,
            );
        }
        Err(_) => return unavailable(lang, "empd"),
    };
    if input.kind == "reference" {
        match cards.items.as_slice() {
            [one] if one.reference == value => {
                return card::view(one, &desk.zone, lang, can_write, None);
            }
            [] => {}
            _ => return unavailable(lang, "empd"),
        }
    }
    for card in &cards.items {
        let label = format!("{} {}", lang.pick("Karte öffnen", "Open card"), card.visual);
        let _ = write!(
            list,
            "<li><p>{}</p>{}</li>",
            card.standing(lang),
            post_button("reference", &card.reference, None, &label, lang),
        );
    }
    matches_page(input, lang, &list, cards.next.as_deref())
}

fn matches_page(input: &Input, lang: Lang, list: &str, cursor: Option<&str>) -> Response {
    let results = if list.is_empty() {
        format!(
            "<p>{}</p>",
            lang.pick("Keine Übereinstimmung.", "No match.")
        )
    } else {
        format!(
            "<section class=\"card\"><h2>{}</h2><ol>{list}</ol></section>",
            lang.pick("Treffer", "Matches")
        )
    };
    let more = cursor.map_or_else(String::new, |c| {
        post_button(
            &input.kind,
            input.value.trim(),
            Some(c),
            lang.pick("Weitere Treffer", "More matches"),
            lang,
        )
    });
    lookup_page(lang, &(results + &more))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_retired_station_and_missing_frames_remain_visible_without_a_dead_link() {
        let facts: Evidence = serde_json::from_value(json!({
            "position": 4, "identity": "<old>", "transaction": "op-2",
            "station_registered": false, "billed": false, "reasons": ["No record"],
            "findings": [], "started_at": null, "ended_at": null, "decided_at": "2026-11-25T08:00:00Z",
            "observed_frames": 0, "missing_frames": 2, "receipt": "//example.test/r/x"
        })).unwrap();
        for (lang, retired, gap, no_card) in [
            (
                Lang::En,
                "no longer registered",
                "recorded timeline is incomplete",
                "No linked card reference",
            ),
            (
                Lang::De,
                "nicht mehr im Stationsregister",
                "Ablauf ist unvollständig",
                "Keine zugeordnete Kartenreferenz",
            ),
        ] {
            let response = render(
                &facts,
                &TimeZone::new("Europe/Berlin").unwrap(),
                "https://receipts.example",
                lang,
            );
            let bytes = axum::body::to_bytes(response.into_body(), 150_000)
                .await
                .unwrap();
            let html = std::str::from_utf8(&bytes).unwrap();
            assert!(html.contains(retired) && html.contains(gap) && html.contains(no_card));
            assert!(
                html.contains("&lt;old&gt;")
                    && !html.contains("/station/")
                    && !html.contains("example.test")
            );
            assert!(html.contains("2026-11-25T08:00:00Z"));
        }
        assert_eq!(
            receipt_url("https://example.test/", "/r/0123456789abcdefghij-_").as_deref(),
            Some("https://example.test/r/0123456789abcdefghij-_")
        );
    }
}
