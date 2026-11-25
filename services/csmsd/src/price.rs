//! The price, before the plug: `/p/{evse}` and `/p/{evse}.atom` (spec 015).
//!
//! `[AFIR Art. 5(4)]` wants the ad-hoc price known before a session starts, in
//! its own order of components, and `[PAngG §3]` fines a price not, not
//! correctly or not completely shown from 01.01.2027. This page is that
//! showing, and it computes nothing: every figure is `emob-tariff`'s — the
//! description from [`emob_tariff::describe_gross`], the examples from
//! [`emob_tariff::worked_total`], which rates them with the function that bills
//! — taken from the very version a session starting now would be rated with.
//!
//! The **Preisstand** — the version's fingerprint, four characters — stands on
//! the page, on the station's display and on the receipt. A driver who sees
//! the same code three times has seen the price that applied; a sticker over
//! the station's own code is what fake-QR fraud looks like, so the page says
//! to scan the screen.
//!
//! Public, served fresh for every request, `noindex`, rate-limited. An EVSE id nobody runs is
//! the shell's empty 404, never echoed.

use std::fmt::Write as _;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as UrlPath, Query, State as Extract};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use emob_core::{EvseId, TimeZone};
use emob_service::journal::Journal;
use emob_service::ui::{self, Lang, Shell, escape};
use emob_tariff::{Dimension, Tariff, TariffHistory};
use rust_decimal::Decimal;

use crate::Csmsd;
use crate::receipt::{Window, money, number, when};

/// Requests per second `/p/` takes before it answers 429.
pub const PER_SECOND: u32 = 60;

/// The examples a driver compares: three sessions they might charge, in kWh.
pub const EXAMPLES: [u32; 3] = [10, 30, 50];

/// Who runs the point, as the page names them.
#[derive(Debug, Clone, Default)]
pub struct Operator {
    /// The operator's name.
    pub name: String,
    /// How to reach them, when the estate says.
    pub contact: Option<String>,
}

struct Shared<J: Journal> {
    csmsd: Arc<Csmsd<J>>,
    operator: Operator,
    window: Window,
}

/// The price routes, unauthenticated.
pub fn router<J: Journal>(csmsd: Arc<Csmsd<J>>, operator: Operator) -> Router {
    Router::new()
        .route("/p/{evse}", get(page::<J>))
        .layer(axum::middleware::map_response(public_price_headers))
        .with_state(Arc::new(Shared {
            csmsd,
            operator,
            window: Window::new(PER_SECOND),
        }))
}

#[derive(serde::Deserialize)]
struct LangQuery {
    lang: Option<String>,
}

/// One point's price, as the routes read it under one lock.
struct Point {
    evse: EvseId,
    power_kw: Decimal,
    site: String,
    zone: TimeZone,
    history: TariffHistory,
    /// `14:30` while the point is reserved — the time only, never for whom
    /// (SR-176).
    held_until: Option<String>,
    provider_links: Vec<(String, String)>,
}

fn find<J: Journal>(csmsd: &Csmsd<J>, evse: &str) -> Option<Point> {
    let evse: EvseId = evse.parse().ok()?;
    let station = csmsd.with_registry(|registry, _| {
        registry
            .values()
            .find(|station| station.publicly_accessible && station.evse == evse)
            .cloned()
    })?;
    Some(Point {
        provider_links: csmsd.plan_links(&evse),
        held_until: csmsd
            .reservation(&station.identity)
            .map(|held| csmsd.held_until(&station.identity, held.until)),
        history: csmsd.tariff(&station.tariff)?.clone(),
        power_kw: station.rated_power_kw,
        site: station.location.name.clone(),
        zone: TimeZone::new(&station.location.time_zone).unwrap_or_else(|_| TimeZone::utc()),
        evse,
    })
}

async fn page<J: Journal>(
    Extract(shared): Extract<Arc<Shared<J>>>,
    UrlPath(path): UrlPath<String>,
    Query(query): Query<LangQuery>,
    headers: HeaderMap,
) -> Response {
    if !shared.window.admits() {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let (evse, feed) = match path.strip_suffix(".atom") {
        Some(evse) => (evse, true),
        None => (path.as_str(), false),
    };
    let Some(point) = find(&shared.csmsd, evse) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let public = shared.csmsd.public_url().unwrap_or_default();
    let now = shared.csmsd.now();
    if feed {
        let mut response = atom(&point, &shared.operator, public).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/atom+xml; charset=utf-8"),
        );
        return response;
    }
    let lang = Lang::negotiate(
        query.lang.as_deref(),
        headers
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok()),
    );
    ui::hardened(
        ui::html(render(&point, &shared.operator, public, now, lang)),
        ui::CSP,
    )
}

/// A hold, retirement, tariff boundary or locale change can alter the next response.
async fn public_price_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.append(header::VARY, HeaderValue::from_static("Accept-Language"));
    headers.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    response
}

/// The four characters a driver compares: page, display, receipt.
#[must_use]
pub fn preisstand(tariff: &Tariff) -> String {
    tariff.fingerprint().code()
}

/// The first line a driver reads, in German: the price per kWh, or "kostenlos".
#[must_use]
pub fn headline(tariff: &Tariff, at: time::OffsetDateTime) -> String {
    lines(tariff, at, Lang::De)
        .into_iter()
        .next()
        .unwrap_or_else(|| "kostenlos".to_owned())
}

/// The page's own address, for the sentence that tells a driver what they
/// should be seeing.
#[must_use]
pub fn path(evse: &EvseId) -> String {
    format!("/p/{evse}")
}

fn unit(dimension: Dimension, lang: Lang) -> &'static str {
    match (dimension, lang) {
        (Dimension::Energy, _) => "kWh",
        (Dimension::Time, Lang::De) => "Minute Laden",
        (Dimension::Time, Lang::En) => "minute charging",
        (Dimension::ParkingTime, Lang::De) => "Minute Belegung",
        (Dimension::ParkingTime, Lang::En) => "minute occupied",
        (Dimension::Flat, Lang::De) => "Ladevorgang",
        (Dimension::Flat, Lang::En) => "session",
    }
}

/// A rate as a driver reads it: every decimal the tariff states, at least the
/// currency's two.
fn rate(price: Decimal, currency: emob_core::Currency, lang: Lang) -> String {
    let mut exact = price.normalize();
    if exact.scale() < 2 {
        exact.rescale(2);
    }
    let symbol = if currency.to_string() == "EUR" {
        "€".to_owned()
    } else {
        currency.to_string()
    };
    format!("{}\u{a0}{symbol}", number(&exact.to_string(), lang))
}

/// The lines a version shows, components in `[AFIR Art. 5(4)]`'s order.
fn lines(tariff: &Tariff, at: time::OffsetDateTime, lang: Lang) -> Vec<String> {
    let Ok(shown) = emob_tariff::describe_gross(tariff, at) else {
        return vec![
            lang.pick(
                "Gesamtpreis derzeit unbekannt",
                "Total price currently unavailable",
            )
            .to_owned(),
        ];
    };
    let format_line = |line: &emob_tariff::display::DisplayLine| {
        format!(
            "{} {} {}",
            rate(line.price, shown.currency, lang),
            lang.pick("pro", "per"),
            unit(line.dimension, lang)
        )
    };
    let mut result: Vec<_> = shown
        .lines
        .iter()
        .filter(|line| !line.price.is_zero())
        .map(format_line)
        .collect();
    for tier in &shown.tiers {
        if !tier.condition.is_empty() {
            let prices = tier
                .lines
                .iter()
                .map(format_line)
                .collect::<Vec<_>>()
                .join(" · ");
            result.push(format!("{} ({})", prices, tier.condition));
        }
    }
    for (bound, label) in [
        (shown.min_price, lang.pick("Mindestpreis", "Minimum")),
        (shown.max_price, lang.pick("Höchstpreis", "Maximum")),
    ] {
        if let Some(value) = bound {
            result.push(format!("{label} {}", rate(value, shown.currency, lang)));
        }
    }
    result
}

#[allow(clippy::too_many_lines)] // One page, top to bottom, in reading order.
fn render(
    point: &Point,
    operator: &Operator,
    public: &str,
    now: time::OffsetDateTime,
    lang: Lang,
) -> String {
    let mut body = format!(
        "<p class=\"eyebrow\">{charge_point} {evse} · {site}</p>",
        charge_point = lang.pick("Ladepunkt", "Charge point"),
        evse = escape(&point.evse.to_string()),
        site = escape(&point.site),
    );
    if let Some(until) = &point.held_until {
        let _ = write!(
            body,
            "<p class=\"why tone-warn\"><strong>{} {}</strong> — {}</p>",
            lang.pick("Reserviert bis", "Reserved until"),
            escape(until),
            lang.pick(
                "bis dahin lädt hier nur, wer reserviert hat. Danach ist der Ladepunkt wieder frei.",
                "until then only the driver who reserved it can charge here. Then it is free again.",
            ),
        );
    }
    let Some(current) = point.history.in_force_at(now) else {
        let _ = write!(
            body,
            "<h1>{}</h1><p>{}</p>",
            lang.pick("Gerade kein Preis", "No price right now"),
            lang.pick(
                "Für diesen Ladepunkt gilt im Moment kein Preis. Bitte nicht laden — der Betreiber ist informiert.",
                "No price is in force at this charge point right now. Please do not charge.",
            )
        );
        return shell(&body, point, lang);
    };
    if emob_tariff::describe_gross(current, now).is_err() {
        let _ = write!(
            body,
            "<h1>{}</h1><p>{}</p>",
            lang.pick(
                "Gesamtpreis derzeit unbekannt",
                "Total price currently unavailable"
            ),
            lang.pick(
                "Bitte vor dem Laden den Betreiber kontaktieren.",
                "Contact the operator before charging."
            )
        );
        return shell(&body, point, lang);
    }
    let shown = lines(current, now, lang);
    let mut shown = shown.into_iter();
    match shown.next() {
        None => {
            let _ = write!(
                body,
                "<h1 class=\"price\">{}</h1>",
                lang.pick("Laden ist hier kostenlos", "Charging here is free")
            );
        }
        Some(first) => {
            let _ = write!(body, "<h1 class=\"price\">{}</h1>", escape(&first));
            let rest: Vec<String> = shown.collect();
            if !rest.is_empty() {
                body.push_str("<ul class=\"components\">");
                for line in rest {
                    let _ = write!(body, "<li>{}</li>", escape(&line));
                }
                body.push_str("</ul>");
            }
            let vat = current
                .elements
                .iter()
                .flat_map(|e| e.components.iter())
                .find_map(|c| c.vat);
            if let Some(vat) = vat {
                let _ = write!(
                    body,
                    "<p class=\"muted\">{} {} %</p>",
                    lang.pick("Endpreise inkl. MwSt.", "Final prices incl. VAT"),
                    escape(&number(&vat.normalize().to_string(), lang)),
                );
            }
            // What a session would cost, rated by the function that bills.
            let _ = write!(
                body,
                "<section aria-labelledby=\"examples-h\"><h2 id=\"examples-h\">{}</h2><dl class=\"examples\">",
                match lang {
                    Lang::De => format!(
                        "So viel kostet eine Ladung bei {} kW",
                        number(&point.power_kw.normalize().to_string(), lang)
                    ),
                    Lang::En =>
                        format!("What a session costs at {} kW", point.power_kw.normalize()),
                },
            );
            for kwh in EXAMPLES {
                let kwh = Decimal::from(kwh);
                if let Some(total) = emob_tariff::worked_total(current, kwh, point.power_kw, now) {
                    let minutes = (kwh * Decimal::from(60) / point.power_kw).ceil();
                    let _ = write!(
                        body,
                        "<dt>{kwh} kWh</dt><dd>{total} <span class=\"muted\">· ≈ {minutes} min</span></dd>",
                        total = escape(&money(total.amount(), total.currency(), lang)),
                    );
                }
            }
            body.push_str("</dl></section>");
        }
    }
    // The code, and how to use it.
    let since = current.valid_from.map_or_else(String::new, |from| {
        format!(
            " · {} {}",
            lang.pick("gültig seit", "in force since"),
            escape(&when(from, &point.zone, lang))
        )
    });
    let _ = write!(
        body,
        "<section class=\"card preisstand\" aria-labelledby=\"check-h\"><h2 id=\"check-h\">{title}</h2>\
         <p class=\"code\"><span class=\"muted\">Preisstand</span> <strong>{code}</strong>{since}</p>\
         <p>{how}</p><p class=\"muted\">{address} <span class=\"url\">{url}</span></p></section>",
        title = lang.pick("Ist das der richtige Preis?", "Is this the right price?"),
        code = escape(&preisstand(current)),
        how = lang.pick(
            "Vergleiche Preis und Preisstand am Ladepunkt mit deinem Beleg. Fehlen Preise auf dem Display, prüfe vor dem Laden das Preisschild. Scanne den Code auf dem Display, nicht einen Aufkleber.",
            "Compare the price and Preisstand shown at the charge point with your receipt. If the screen omits prices, check the price sign before charging. Scan the code on the display, not a sticker.",
        ),
        address = lang.pick("Diese Seite liegt unter", "This page lives at"),
        url = escape(&format!("{public}{}", path(&point.evse))),
    );
    // What comes next, announced.
    for upcoming in point
        .history
        .versions()
        .iter()
        .filter(|version| version.valid_from.is_some_and(|from| from > now))
    {
        let Some(from) = upcoming.valid_from else {
            continue;
        };
        let next = lines(upcoming, from, lang);
        let _ = write!(
            body,
            "<p class=\"next\"><strong>{ab} {at}:</strong> {prices} <span class=\"muted\">(Preisstand {code})</span></p>",
            ab = lang.pick("Ab", "From"),
            at = escape(&when(from, &point.zone, lang)),
            prices = escape(&if next.is_empty() {
                lang.pick("kostenlos", "free").to_owned()
            } else {
                next.join(" · ")
            }),
            code = escape(&preisstand(upcoming)),
        );
    }
    let contact = operator
        .contact
        .as_deref()
        .map_or_else(String::new, |contact| format!(" · {}", escape(contact)));
    let _ = write!(
        body,
        "<p class=\"muted\">{by} {name}{contact} · <a href=\"{feed}\">{subscribe}</a></p>",
        by = lang.pick("Betrieben von", "Operated by"),
        name = escape(&operator.name),
        feed = escape(&format!("{}.atom", path(&point.evse))),
        subscribe = lang.pick("Preisänderungen als Feed", "Price changes as a feed"),
    );
    if !point.provider_links.is_empty() {
        let _ = write!(
            body,
            "<section><h2>{}</h2><ul>",
            lang.pick("Vertragspreise", "Contract prices")
        );
        for (url, label) in &point.provider_links {
            let _ = write!(
                body,
                "<li><a href=\"{}\">{}</a></li>",
                escape(url),
                escape(label)
            );
        }
        body.push_str("</ul></section>");
    }
    shell(&body, point, lang)
}

fn shell(body: &str, point: &Point, lang: Lang) -> String {
    let title = match lang {
        Lang::De => format!("Preis · {}", point.evse),
        Lang::En => format!("Price · {}", point.evse),
    };
    ui::page(
        &Shell {
            title: &title,
            place: lang.pick("Preis", "Price"),
            lang,
            reading: true,
            script: None,
        },
        body,
    )
}

/// One entry per version, the announced ones included — for comparison
/// sites and the people who check prices by hand today.
fn atom(point: &Point, operator: &Operator, public: &str) -> String {
    let rfc3339 = |at: time::OffsetDateTime| {
        at.format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default()
    };
    // A version without a start has been in force from the beginning.
    let start = |tariff: &Tariff| {
        tariff
            .valid_from
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
    };
    let updated = point
        .history
        .versions()
        .iter()
        .map(start)
        .max()
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let page = format!("{public}{}", path(&point.evse));
    let mut feed = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<feed xmlns=\"http://www.w3.org/2005/Atom\">\
         <title>Preis am Ladepunkt {evse}</title><id>{page}</id>\
         <link rel=\"self\" href=\"{page}.atom\"/><link href=\"{page}\"/>\
         <updated>{updated}</updated><author><name>{name}</name></author>",
        evse = escape(&point.evse.to_string()),
        page = escape(&page),
        updated = rfc3339(updated),
        name = escape(&operator.name),
    );
    for version in point.history.versions().iter().rev() {
        let at = start(version);
        let shown = lines(version, at, Lang::De);
        let _ = write!(
            feed,
            "<entry><id>urn:emob:tariff:{fingerprint}</id><title>Preisstand {code}: {first}</title>\
             <updated>{at}</updated><content type=\"text\">{all}</content></entry>",
            fingerprint = version.fingerprint(),
            code = escape(&preisstand(version)),
            first = escape(shown.first().map_or("kostenlos", String::as_str)),
            at = rfc3339(at),
            all = escape(&shown.join(" · ")),
        );
    }
    feed.push_str("</feed>\n");
    feed
}

// ── On the station's own display ────────────────────────────────────────────

/// One display message per version still to come or in force, shown over its
/// window, so the station changes its own screen when the price does.
struct Shown {
    id: i32,
    text: String,
    from: Option<time::OffsetDateTime>,
    until: Option<time::OffsetDateTime>,
    fingerprint: String,
}

fn shown(identity: &str, history: &TariffHistory, now: time::OffsetDateTime) -> Vec<Shown> {
    history
        .versions()
        .iter()
        .filter(|version| version.valid_until.is_none_or(|until| until > now))
        .map(|version| {
            let at = version.valid_from.filter(|from| *from > now).unwrap_or(now);
            // A station's display may not know a no-break space.
            let components = lines(version, at, Lang::De);
            let disclosure = if components.is_empty() {
                "kostenlos".to_owned()
            } else {
                components.join(" · ")
            };
            let first = disclosure.replace('\u{a0}', " ");
            let fingerprint = version.fingerprint().to_string();
            Shown {
                id: crate::display::message_id(identity, &format!("price-{fingerprint}")),
                text: format!("{first} · Preisstand {}", preisstand(version)),
                from: version.valid_from.filter(|from| *from > now),
                until: version.valid_until,
                fingerprint,
            }
        })
        .collect()
}

// OCPP 2.0.1 and 2.1 SetDisplayMessageRequest schemas, MessageContentType.content.
const DISPLAY_LIMIT_201: usize = 512;
const DISPLAY_LIMIT_21: usize = 1024;
const CHECK_SIGN: &str = "Vollständigen Preis vor dem Laden auf dem Preisschild prüfen. Fehlt er, Betreiber kontaktieren.";

fn display_text(text: String, link: Option<&str>, limit: usize) -> String {
    let length = text.chars().count();
    if length > limit {
        return CHECK_SIGN.to_owned();
    }
    if let Some(url) = link
        && length + 3 + url.chars().count() <= limit
    {
        return format!("{text} · {url}");
    }
    text
}

/// The 2.0.1 messages: the price, the Preisstand and the page's address as
/// text, in the normal cycle — a receipt link still goes in front.
#[must_use]
pub fn messages_v201(
    identity: &str,
    history: &TariffHistory,
    url: &str,
    now: time::OffsetDateTime,
) -> Vec<ocpp_kit::v2_0_1::SetDisplayMessageRequest> {
    use ocpp_kit::v2_0_1::{MessageContent, MessageFormat, MessageInfo, MessagePriority};
    shown(identity, history, now)
        .into_iter()
        .map(|message| {
            let mut info = MessageInfo::new(
                message.id,
                MessagePriority::NormalCycle,
                MessageContent::new(
                    MessageFormat::UTF8,
                    display_text(message.text, Some(url), DISPLAY_LIMIT_201),
                )
                .with_language("de"),
            );
            info.start_date_time = message.from.and_then(crate::display::until);
            info.end_date_time = message.until.and_then(crate::display::until);
            ocpp_kit::v2_0_1::SetDisplayMessageRequest::new(info)
        })
        .collect()
}

/// The 2.1 messages: the price and the Preisstand as text, and the page's
/// address as the station's own QR code (`QRCODE`) — the code a driver
/// should scan instead of a sticker.
#[must_use]
pub fn messages_v21(
    identity: &str,
    history: &TariffHistory,
    url: &str,
    now: time::OffsetDateTime,
) -> Vec<ocpp_kit::v2_1::SetDisplayMessageRequest> {
    use ocpp_kit::v2_1::{MessageContent, MessageFormat, MessageInfo, MessagePriority};
    shown(identity, history, now)
        .into_iter()
        .flat_map(|message| {
            let text = MessageContent::new(
                MessageFormat::UTF8,
                display_text(message.text, None, DISPLAY_LIMIT_21),
            )
            .with_language("de");
            let qr = (url.chars().count() <= DISPLAY_LIMIT_21).then(|| {
                let id = crate::display::message_id(
                    identity,
                    &format!("price-{}-qr", message.fingerprint),
                );
                (
                    id,
                    MessageContent::new(MessageFormat::QRCODE, url.to_owned()),
                )
            });
            std::iter::once((message.id, text))
                .chain(qr)
                .map(move |(id, content)| {
                    let mut info = MessageInfo::new(id, MessagePriority::NormalCycle, content);
                    info.start_date_time = message.from.and_then(crate::display::until);
                    info.end_date_time = message.until.and_then(crate::display::until);
                    ocpp_kit::v2_1::SetDisplayMessageRequest::new(info)
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_tariff::{PriceComponent, TariffKind};
    use time::macros::datetime;

    fn version(
        energy: &str,
        from: Option<time::OffsetDateTime>,
        until: Option<time::OffsetDateTime>,
    ) -> Tariff {
        Tariff::simple(
            "ad-hoc".parse().unwrap(),
            emob_core::Currency::EUR,
            TariffKind::AdHoc,
            TimeZone::new("Europe/Berlin").unwrap(),
            vec![
                PriceComponent::new(Dimension::Flat, Decimal::new(50, 2))
                    .with_vat(Decimal::from(19)),
                PriceComponent::new(Dimension::Energy, energy.parse().unwrap())
                    .with_vat(Decimal::from(19)),
            ],
        )
        .valid_between(from, until)
    }

    fn point() -> Point {
        let change = datetime!(2027-01-01 00:00 +1);
        Point {
            evse: "DE*MUS*E00003".parse().unwrap(),
            power_kw: Decimal::from(150),
            site: "Betriebshof <Mitte>".to_owned(),
            zone: TimeZone::new("Europe/Berlin").unwrap(),
            history: TariffHistory::new(vec![
                version("0.59", None, Some(change)),
                version("0.52", Some(change), None),
            ])
            .unwrap(),
            held_until: None,
            provider_links: Vec::new(),
        }
    }

    fn operator() -> Operator {
        Operator {
            name: "Stadtwerke Musterstadt".to_owned(),
            contact: Some("service@example.org".to_owned()),
        }
    }

    #[test]
    fn sr_176_a_held_point_says_until_when_and_nothing_about_whom() {
        let held = Point {
            held_until: Some("14:30".to_owned()),
            ..point()
        };
        let html = render(
            &held,
            &operator(),
            "https://p.example",
            datetime!(2026-12-01 10:00 +1),
            Lang::De,
        );
        assert!(html.contains("Reserviert bis 14:30</strong>"), "{html}");
        assert!(!html.contains("Karte") && !html.contains("operator:"));
    }

    #[test]
    fn the_price_per_kwh_is_the_heading_and_the_components_follow_in_afir_order() {
        let html = render(
            &point(),
            &operator(),
            "https://p.example",
            datetime!(2026-12-01 10:00 +1),
            Lang::De,
        );
        assert!(
            html.contains("<h1 class=\"price\">0,59\u{a0}€ pro kWh</h1>"),
            "{html}"
        );
        assert!(
            html.contains("<li>0,50\u{a0}€ pro Ladevorgang</li>"),
            "{html}"
        );
        // Rated by the billing function: 30 kWh × 0.59 + 0.50.
        assert!(html.contains("<dt>30 kWh</dt><dd>18,20\u{a0}€"), "{html}");
        assert!(html.contains("Endpreise inkl. MwSt. 19 %"));
        assert!(html.contains("Betriebshof &lt;Mitte&gt;"), "escaped");
    }

    #[test]
    fn the_preisstand_is_shown_with_the_announced_change_and_where_the_page_lives() {
        let p = point();
        let now = datetime!(2026-12-01 10:00 +1);
        let html = render(&p, &operator(), "https://p.example", now, Lang::De);
        let current = preisstand(p.history.in_force_at(now).unwrap());
        assert_eq!(current.len(), 4);
        assert!(
            html.contains(&format!("<strong>{current}</strong>")),
            "{html}"
        );
        assert!(
            html.contains("Ab 01.01.2027, 00:00:</strong> 0,52\u{a0}€ pro kWh"),
            "{html}"
        );
        assert!(html.contains("https://p.example/p/DE*MUS*E00003"));
        assert!(html.contains("nicht einen Aufkleber"));
        assert!(html.len() < 30_000, "SC-003: {} bytes", html.len());
    }

    #[test]
    fn sr_209_the_station_discloses_the_occupancy_fee_and_an_unknown_gross_price_never_reads_as_free()
     {
        let now = datetime!(2026-12-01 10:00 +1);
        let mut tariff = version("0.59", None, None);
        tariff.elements[0].components.push(
            PriceComponent::new(Dimension::ParkingTime, Decimal::from(6))
                .with_vat(Decimal::from(19)),
        );
        let history = TariffHistory::new(vec![tariff.clone()]).unwrap();
        let shown = messages_v21("CP-2", &history, "https://example.test/p/DE*ABC*E1", now);
        let text = &shown[0].message.message.content;
        assert!(
            text.contains("0,59 € pro kWh") && text.contains("0,10 € pro Minute Belegung"),
            "{text}"
        );
        let en = render(
            &Point { history, ..point() },
            &operator(),
            "",
            now,
            Lang::En,
        );
        assert!(en.contains("per minute occupied"));
        // A long URL cannot crowd prices off a screen or create an invalid QR payload.
        let long_link = format!("https://example.test/{}", "x".repeat(1100));
        let prices = TariffHistory::single(tariff.clone()).unwrap();
        let short = messages_v201("CP-2", &prices, &long_link, now);
        assert!(
            short[0]
                .message
                .message
                .content
                .contains("0,10 € pro Minute Belegung")
        );
        assert!(!short[0].message.message.content.contains(&long_link));
        assert_eq!(messages_v21("CP-2", &prices, &long_link, now).len(), 1);
        // Conditional rates must never be truncated into a partial price.
        let mut tiered = tariff.clone();
        let template = tiered.elements[0].clone();
        for threshold in 1..80 {
            let mut tier = template.clone();
            tier.restrictions.min_kwh = Some(Decimal::from(threshold));
            tiered.elements.push(tier);
        }
        let complex = TariffHistory::single(tiered).unwrap();
        assert_eq!(
            messages_v201("CP-2", &complex, "https://example.test", now)[0]
                .message
                .message
                .content,
            CHECK_SIGN
        );
        assert_eq!(
            messages_v21("CP-2", &complex, "https://example.test", now)[0]
                .message
                .message
                .content,
            CHECK_SIGN
        );
        tariff.tax_included = emob_tariff::TaxIncluded::No;
        tariff.elements[0].components[0].vat = None;
        let unknown = Point {
            history: TariffHistory::new(vec![tariff]).unwrap(),
            ..point()
        };
        assert!(
            render(&unknown, &operator(), "", now, Lang::En)
                .contains("Total price currently unavailable")
        );
        assert!(!atom(&unknown, &operator(), "").contains("kostenlos"));
    }

    #[test]
    fn the_feed_has_one_entry_per_version_newest_first() {
        let feed = atom(&point(), &operator(), "https://p.example");
        assert_eq!(feed.matches("<entry>").count(), 2);
        assert!(
            feed.find("0,52\u{a0}€") < feed.find("0,59\u{a0}€"),
            "{feed}"
        );
        assert!(
            feed.contains("<updated>2027-01-01T00:00:00+01:00</updated>"),
            "{feed}"
        );
    }

    #[test]
    fn the_display_gets_each_version_over_its_window_and_a_qr_code_on_two_one() {
        let now = datetime!(2026-12-01 10:00 +1);
        let p = point();
        let url = "https://p.example/p/DE*MUS*E00003";
        let v201 = messages_v201("CP-2", &p.history, url, now);
        assert_eq!(v201.len(), 2, "the current and the announced version");
        let current = &v201[0].message;
        assert!(
            current
                .message
                .content
                .starts_with("0,59 € pro kWh · 0,50 € pro Ladevorgang · Preisstand "),
            "{:?}",
            current.message.content
        );
        assert!(current.message.content.ends_with(url));
        assert!(current.start_date_time.is_none(), "in force now");
        assert!(current.end_date_time.is_some(), "until the change");
        assert!(v201[1].message.start_date_time.is_some(), "from the change");
        let v21 = messages_v21("CP-3", &p.history, url, now);
        assert_eq!(v21.len(), 4);
        assert!(v21.iter().any(|m| m.message.message.content == url));
        let ids: std::collections::BTreeSet<i32> = v21.iter().map(|m| m.message.id).collect();
        assert_eq!(ids.len(), 4, "each message replaces only itself");
    }

    #[test]
    fn a_point_with_no_price_in_force_says_so_and_shows_no_zero() {
        let mut p = point();
        p.history = TariffHistory::new(vec![version(
            "0.59",
            Some(datetime!(2027-06-01 00:00 +1)),
            None,
        )])
        .unwrap();
        let html = render(
            &p,
            &operator(),
            "",
            datetime!(2026-12-01 10:00 +1),
            Lang::De,
        );
        assert!(html.contains("Gerade kein Preis"), "{html}");
        assert!(!html.contains("0,00"));
    }
}
