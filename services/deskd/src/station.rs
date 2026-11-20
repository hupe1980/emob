//! The station page: one station as the platform knows it (spec 012).
//!
//! Verdict first — online since when, or silent since when — then the asset
//! card: what the estate says it is, what the station said at its last boot,
//! what its meter signs about itself, and every firmware change, which is the
//! intervention record `[MessEG §31(2) Nr. 4]` makes the operator keep. Then
//! how it is doing and what it did. Everything the station supplied goes
//! through [`escape`] (SR-137); the page computes nothing that decides.

use core::fmt::Write as _;

use emob_core::TimeZone;
use emob_service::ui::{self, Lang, escape};
use emob_session::success::Rate;
use serde::Deserialize;

/// `csmsd`'s station view, as this page reads it.
#[derive(Debug, Clone, Deserialize)]
pub struct StationPage {
    /// The OCPP identity.
    pub identity: String,
    /// The point.
    pub evse: String,
    /// `1.6`, `2.0.1`, `2.1`.
    pub ocpp: String,
    /// Security profile.
    pub security_profile: u8,
    /// Rated power, kW.
    pub rated_power_kw: String,
    /// In service since.
    pub commissioned: Option<String>,
    /// Its tariff.
    #[serde(default)]
    pub tariff: String,
    /// Its connector.
    #[serde(default)]
    pub connector: String,
    /// Minutes its display shows a receipt's code.
    #[serde(default)]
    pub receipt_display_minutes: Option<u8>,
    /// Where it stands.
    pub location: Option<Place>,
    /// Online or offline.
    pub presence: Option<Presence>,
    /// What it said at boot.
    pub reported_identity: Option<Reported>,
    /// Firmware changes, newest first.
    #[serde(default)]
    pub firmware_history: Vec<Change>,
    /// Meters, as their records name them.
    #[serde(default)]
    pub meters: Vec<Meter>,
    /// Reports, newest first.
    #[serde(default)]
    pub reports: Vec<Report>,
    /// Boots in seven days.
    #[serde(default)]
    pub boots_7d: usize,
    /// Success today and over seven days.
    pub success: Success,
    /// Last sessions, newest first.
    #[serde(default)]
    pub recent: Vec<Recent>,
}

/// Where a station stands, as the registry holds it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Place {
    /// The location's id.
    #[serde(default)]
    pub id: String,
    /// The site's name.
    pub name: String,
    /// Street.
    pub street: String,
    /// Number.
    pub house_number: String,
    /// Postal code.
    pub postal_code: String,
    /// City.
    pub city: String,
    /// ISO 3166-1 alpha-2.
    pub country: String,
    /// Degrees north, as decimal text.
    pub latitude: String,
    /// Degrees east, as decimal text.
    pub longitude: String,
    /// IANA zone.
    pub time_zone: String,
}

/// Presence.
#[derive(Debug, Clone, Deserialize)]
pub struct Presence {
    /// `online` or `offline`.
    pub state: String,
    /// Since when.
    #[serde(with = "time::serde::rfc3339")]
    pub since: time::OffsetDateTime,
    /// Last heard, when offline.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_heard: Option<time::OffsetDateTime>,
}

/// The station's word at boot.
#[derive(Debug, Clone, Deserialize)]
pub struct Reported {
    /// Manufacturer.
    pub vendor: String,
    /// Model.
    pub model: String,
    /// Serial.
    pub serial: Option<String>,
    /// Firmware.
    pub firmware: Option<String>,
    /// Its mobile network, MCC-MNC.
    #[serde(default)]
    pub network: Option<String>,
    /// Who runs it.
    #[serde(default)]
    pub carrier: Option<String>,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
}

/// A firmware change.
#[derive(Debug, Clone, Deserialize)]
pub struct Change {
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
    /// `station` or a point.
    pub component: String,
    /// Before.
    pub from: Option<String>,
    /// After.
    pub to: String,
}

/// A meter, as it signs.
#[derive(Debug, Clone, Deserialize)]
pub struct Meter {
    /// The point.
    pub evse: String,
    /// `MV`.
    pub vendor: Option<String>,
    /// `MM`.
    pub model: Option<String>,
    /// `MS`.
    pub serial: Option<String>,
    /// `MF`.
    pub firmware: Option<String>,
    /// `GV`.
    pub gateway_version: Option<String>,
    /// Since.
    #[serde(with = "time::serde::rfc3339")]
    pub since: time::OffsetDateTime,
    /// Whether the registry holds its key.
    pub key_registered: bool,
}

/// A report.
#[derive(Debug, Clone, Deserialize)]
pub struct Report {
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
    /// `firmware` or `security`.
    pub kind: String,
    /// A firmware status.
    pub status: Option<String>,
    /// A security event.
    pub event: Option<String>,
    /// Its detail.
    pub detail: Option<String>,
}

/// Success.
#[derive(Debug, Clone, Deserialize)]
pub struct Success {
    /// Today.
    pub today: Option<Rate>,
    /// Seven days.
    pub week: Option<Rate>,
}

/// A session.
#[derive(Debug, Clone, Deserialize)]
pub struct Recent {
    /// The transaction.
    pub transaction: String,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
    /// Billed or not.
    pub billed: bool,
    /// The receipt path.
    pub receipt: Option<String>,
}

/// What every section formats with.
struct Ctx<'a> {
    lang: Lang,
    zone: &'a TimeZone,
    now: time::OffsetDateTime,
}

impl Ctx<'_> {
    /// An instant as the page shows it: the clock today, the date before.
    fn when(&self, at: time::OffsetDateTime) -> String {
        let local = self.zone.local(at);
        let clock = format!("{:02}:{:02}", local.time.hour(), local.time.minute());
        let shown = if local.date == self.zone.local(self.now).date {
            clock
        } else {
            format!("{} {clock}", self.lang.date(local.date))
        };
        format!(
            "<time datetime=\"{}\">{shown}</time>",
            escape(
                &at.format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default()
            )
        )
    }
}

fn text(value: Option<&String>) -> String {
    value.map_or_else(|| "—".to_owned(), |v| escape(v))
}

/// The page.
#[must_use]
pub fn render(
    page: &StationPage,
    lang: Lang,
    zone: &TimeZone,
    now: time::OffsetDateTime,
    receipts: &str,
) -> String {
    let ctx = Ctx { lang, zone, now };
    let mut body = verdict(page, &ctx);
    body.push_str(&rate(page, &ctx));
    body.push_str(&asset(page, &ctx));
    body.push_str(&place(page, &ctx));
    body.push_str(&meters(page, &ctx));
    body.push_str(&firmware(page, &ctx));
    body.push_str(&reports(page, &ctx));
    body.push_str(&recent(page, &ctx, receipts));
    body.push_str(&manage(page, &ctx));
    ui::page(
        &ui::Shell {
            title: &page.identity,
            place: lang.pick("Station", "Station"),
            lang,
            reading: true,
            script: None,
        },
        &body,
    )
}

/// Online since when, silent since when, or never heard.
fn verdict(page: &StationPage, ctx: &Ctx<'_>) -> String {
    let lang = ctx.lang;
    let (tone, icon, headline, detail) = match &page.presence {
        Some(p) if p.state == "online" => (
            "tone-good",
            "●",
            lang.pick("Online", "Online"),
            format!("{} {}", lang.pick("seit", "since"), ctx.when(p.since)),
        ),
        Some(p) => (
            "tone-warn",
            "◌",
            lang.pick("Still", "Silent"),
            format!(
                "{} {}",
                lang.pick("zuletzt gehört", "last heard"),
                ctx.when(p.last_heard.unwrap_or(p.since))
            ),
        ),
        None => (
            "tone-info",
            "○",
            lang.pick("Noch nie gehört", "Never heard"),
            lang.pick(
                "Seit dieser Station im Bestand ist, hat sie sich nicht gemeldet.",
                "It has not connected since it was added to the estate.",
            )
            .to_owned(),
        ),
    };
    format!(
        "<p class=\"eyebrow back\"><a href=\"/\">{back}</a></p>\
         <div class=\"verdict {tone}\"><span class=\"icon\" aria-hidden=\"true\">{icon}</span>\
         <div><h1>{identity} · {headline}</h1><p>{detail}</p></div></div>",
        back = lang.pick("← Heute", "← Today"),
        identity = escape(&page.identity),
    )
}

/// Success today and over seven days, and reboots.
fn rate(page: &StationPage, ctx: &Ctx<'_>) -> String {
    let lang = ctx.lang;
    let stat = |label: &str, rate: Option<&Rate>| {
        let value = rate.and_then(Rate::percent).map_or_else(
            || "—".to_owned(),
            |p| format!("{} %", lang.decimal(p.round_dp(1))),
        );
        let of = rate.map_or_else(String::new, |r| {
            format!(
                "<span class=\"muted\">{} {} {}</span>",
                r.billed,
                lang.pick("von", "of"),
                r.attempted
            )
        });
        format!(
            "<div class=\"stat\"><span class=\"muted\">{label}</span><span class=\"value\">{value}</span>{of}</div>"
        )
    };
    format!(
        "<section class=\"card\" aria-labelledby=\"rate-h\"><h2 id=\"rate-h\">{title}</h2><div class=\"stats\">{today}{week}\
         <div class=\"stat\"><span class=\"muted\">{boots_l}</span><span class=\"value\">{boots}</span></div></div></section>",
        title = lang.pick("Ladeerfolg", "Charging success"),
        today = stat(lang.pick("Heute", "Today"), page.success.today.as_ref()),
        week = stat(lang.pick("7 Tage", "7 days"), page.success.week.as_ref()),
        boots_l = lang.pick("Neustarts, 7 Tage", "Reboots, 7 days"),
        boots = page.boots_7d,
    )
}

/// What the estate and the station say it is.
fn asset(page: &StationPage, ctx: &Ctx<'_>) -> String {
    let lang = ctx.lang;
    let row = |dt: &str, dd: String| format!("<dt>{dt}</dt><dd>{dd}</dd>");
    let mut card = String::new();
    card.push_str(&row("EVSE", escape(&page.evse)));
    card.push_str(&row(
        lang.pick("Leistung", "Power"),
        format!("{} kW", escape(&page.rated_power_kw)),
    ));
    card.push_str(&row(
        "OCPP",
        format!(
            "{} · {} {}",
            escape(&page.ocpp),
            lang.pick("Sicherheitsprofil", "security profile"),
            page.security_profile
        ),
    ));
    if let Some(reported) = &page.reported_identity {
        card.push_str(&row(
            lang.pick("Hersteller, Modell", "Vendor, model"),
            format!("{} {}", escape(&reported.vendor), escape(&reported.model)),
        ));
        card.push_str(&row(
            lang.pick("Seriennummer", "Serial"),
            text(reported.serial.as_ref()),
        ));
        card.push_str(&row(
            "Firmware",
            format!(
                "{} <span class=\"muted\">· {} {}</span>",
                text(reported.firmware.as_ref()),
                lang.pick("gemeldet beim Start", "reported at boot"),
                ctx.when(reported.at)
            ),
        ));
        if let Some(network) = &reported.network {
            // "262 01": the country code and the network, as a carrier's
            // support line reads it back.
            let code = format!(
                "{} {}",
                &network[..3.min(network.len())],
                network.get(3..).unwrap_or("")
            );
            card.push_str(&row(
                lang.pick("Mobilfunknetz", "Mobile network"),
                match &reported.carrier {
                    Some(carrier) => format!(
                        "<a href=\"/stations?q={network}\">{carrier}</a> <span class=\"muted num\">{code}</span>",
                        network = escape(network),
                        carrier = escape(carrier),
                        code = escape(&code),
                    ),
                    None => format!(
                        "<a href=\"/stations?q={network}\" class=\"num\">{code}</a>",
                        network = escape(network),
                        code = escape(&code),
                    ),
                },
            ));
        }
    }
    if let Some(day) = page.commissioned.as_deref().and_then(|d| {
        time::Date::parse(d, &time::format_description::well_known::Iso8601::DATE).ok()
    }) {
        let years = (ctx.zone.local(ctx.now).date - day).whole_days() / 365;
        card.push_str(&row(
            lang.pick("In Betrieb seit", "In service since"),
            format!(
                "{} <span class=\"muted\">· {years} {}</span>",
                lang.date(day),
                lang.pick(
                    if years == 1 { "Jahr" } else { "Jahre" },
                    if years == 1 { "year" } else { "years" }
                )
            ),
        ));
    }
    format!(
        "<section class=\"card\" aria-labelledby=\"asset-h\"><h2 id=\"asset-h\">{title}</h2><dl class=\"kv\">{card}</dl></section>",
        title = lang.pick("Anlage", "Asset"),
    )
}

/// Where it stands — and a way to see it on a map that the page does not load:
/// the links leave for OpenStreetMap or the phone's own maps app only when the
/// operator follows them, so no tile, script or tracker is ever fetched here.
fn place(page: &StationPage, ctx: &Ctx<'_>) -> String {
    let lang = ctx.lang;
    let Some(place) = &page.location else {
        return String::new();
    };
    // Coordinates go into a URL, so only digits, a sign and a point pass.
    let number = |text: &str| -> Option<String> {
        let clean: String = text.trim().to_owned();
        (!clean.is_empty()
            && clean
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.' || c == '-'))
        .then_some(clean)
    };
    let links = match (number(&place.latitude), number(&place.longitude)) {
        (Some(lat), Some(lon)) => format!(
            "<p class=\"downloads\"><a class=\"btn\" href=\"https://www.openstreetmap.org/?mlat={lat}&amp;mlon={lon}#map=18/{lat}/{lon}\" \
             rel=\"noopener noreferrer\" target=\"_blank\">{osm}</a> <a href=\"geo:{lat},{lon}\">{phone}</a></p>",
            osm = lang.pick("In OpenStreetMap öffnen", "Open in OpenStreetMap"),
            phone = lang.pick("In der Karten-App öffnen", "Open in the maps app"),
        ),
        _ => String::new(),
    };
    let (map, warning) = match (
        crate::map::hundredths(&place.longitude),
        crate::map::hundredths(&place.latitude),
    ) {
        (Some(lon), Some(lat)) => (
            crate::map::locator(lon, lat, lang),
            misplaced(
                crate::map::lies(&place.country, lon, lat),
                &place.country,
                &href(&page.identity),
                lang,
            ),
        ),
        _ => (String::new(), String::new()),
    };
    format!(
        "<section class=\"card\" aria-labelledby=\"place-h\"><h2 id=\"place-h\">{title}</h2>{warning}{map}\
         <dl class=\"kv\"><dt>{name_l}</dt><dd>{name}</dd><dt>{address_l}</dt><dd>{street} {number}, {postal} {city}, {country}</dd>\
         <dt>{coords_l}</dt><dd class=\"num\">{lat}, {lon}</dd><dt>{zone_l}</dt><dd>{zone}</dd></dl>{links}</section>",
        title = lang.pick("Standort", "Location"),
        name_l = lang.pick("Name", "Name"),
        address_l = lang.pick("Adresse", "Address"),
        coords_l = lang.pick("Koordinaten", "Coordinates"),
        zone_l = lang.pick("Zeitzone", "Time zone"),
        name = escape(&place.name),
        street = escape(&place.street),
        number = escape(&place.house_number),
        postal = escape(&place.postal_code),
        city = escape(&place.city),
        country = escape(&place.country),
        lat = escape(&place.latitude),
        lon = escape(&place.longitude),
        zone = escape(&place.time_zone),
    )
}

/// Coordinates that lie outside the station's country say so, and say how to
/// fix them (spec 022). A warning, never a refusal: the outline is simplified.
fn misplaced(lies: crate::map::Lies, country: &str, station: &str, lang: Lang) -> String {
    let crate::map::Lies::Outside { swapped } = lies else {
        return String::new();
    };
    let (what, next) = if swapped {
        (
            lang.pick(
                "Breite und Länge sind vermutlich vertauscht",
                "Latitude and longitude look swapped",
            ),
            lang.pick(
                "Getauscht läge der Punkt im Land.",
                "Exchanged, the point would lie in the country.",
            ),
        )
    } else {
        (
            lang.pick(
                "Die Koordinaten liegen nicht im angegebenen Land",
                "The coordinates do not lie in the stated country",
            ),
            lang.pick(
                "Vorzeichen und Dezimalpunkt prüfen — jede Karte, auf der Fahrende die Station suchen, zeigt diesen Punkt.",
                "Check the sign and the decimal point — every map drivers find this station on shows this point.",
            ),
        )
    };
    format!(
        "<p class=\"callout tone-warn\" role=\"note\"><span aria-hidden=\"true\">⚠</span> <strong>{what} ({country}).</strong> {next} \
         <a class=\"noprint\" href=\"{station}/edit\">{fix}</a></p>",
        country = escape(country),
        station = escape(station),
        fix = lang.pick("Koordinaten ändern", "Change the coordinates"),
    )
}

/// What each point's meter signs about itself.
fn meters(page: &StationPage, ctx: &Ctx<'_>) -> String {
    let lang = ctx.lang;
    if page.meters.is_empty() {
        return String::new();
    }
    let mut rows = String::new();
    for meter in &page.meters {
        let _ = write!(
            rows,
            "<tr><td>{evse}</td><td>{vendor} {model}</td><td>{serial}</td><td>{firmware}</td><td>{key}</td></tr>",
            evse = escape(&meter.evse),
            vendor = text(meter.vendor.as_ref()),
            model = text(meter.model.as_ref()),
            serial = text(meter.serial.as_ref()),
            firmware = text(meter.firmware.as_ref().or(meter.gateway_version.as_ref())),
            key = if meter.key_registered {
                format!(
                    "<span class=\"badge tone-good\">✓ {}</span>",
                    lang.pick("registriert", "registered")
                )
            } else {
                format!(
                    "<span class=\"badge tone-bad\">✕ {}</span>",
                    lang.pick("kein Schlüssel", "no key")
                )
            },
        );
    }
    format!(
        "<section class=\"card\" aria-labelledby=\"meter-h\"><h2 id=\"meter-h\">{title}</h2>\
         <p class=\"muted\">{says}</p><div class=\"scroll\"><table><thead><tr><th>EVSE</th><th>{model}</th>\
         <th>{serial}</th><th>Firmware</th><th>{key}</th></tr></thead><tbody>{rows}</tbody></table></div></section>",
        title = lang.pick("Zähler", "Meter"),
        says = lang.pick(
            "Aus den signierten Messwerten der letzten abgerechneten Sitzung — nicht aus der Selbstauskunft der Station.",
            "From the signed values of the last billed session — not from what the station says about itself.",
        ),
        model = lang.pick("Hersteller, Modell", "Vendor, model"),
        serial = lang.pick("Seriennummer", "Serial"),
        key = lang.pick("Schlüssel", "Key"),
    )
}

/// The intervention record.
fn firmware(page: &StationPage, ctx: &Ctx<'_>) -> String {
    let lang = ctx.lang;
    let mut out = format!(
        "<section class=\"card\" aria-labelledby=\"fw-h\"><h2 id=\"fw-h\">{title}</h2><p class=\"muted\">{why} <span class=\"cite\">MessEG §31(2) Nr. 4</span></p>",
        title = lang.pick("Firmware-Änderungen", "Firmware changes"),
        why = lang.pick(
            "Jede Änderung ist ein Eingriff, dessen Nachweis der Betreiber aufbewahrt.",
            "Every change is an intervention whose record the operator keeps.",
        ),
    );
    if page.firmware_history.is_empty() {
        let _ = write!(
            out,
            "<p class=\"empty\">{}</p>",
            lang.pick("Keine Änderung erfasst.", "No change recorded.")
        );
    } else {
        out.push_str("<ol class=\"values\">");
        for change in &page.firmware_history {
            let _ = write!(
                out,
                "<li><span class=\"what\">{from} → {to}</span><span class=\"muted\">{component}</span> {at}</li>",
                from = text(change.from.as_ref()),
                to = escape(&change.to),
                component = if change.component == "station" {
                    lang.pick("Station", "Station").to_owned()
                } else {
                    format!(
                        "{} {}",
                        lang.pick("Zähler", "Meter"),
                        escape(&change.component)
                    )
                },
                at = ctx.when(change.at),
            );
        }
        out.push_str("</ol>");
    }
    out.push_str("</section>");
    out
}

/// What the station reported.
fn reports(page: &StationPage, ctx: &Ctx<'_>) -> String {
    if page.reports.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "<section class=\"card\" aria-labelledby=\"rep-h\"><h2 id=\"rep-h\">{}</h2><ol class=\"values\">",
        ctx.lang.pick("Meldungen der Station", "Station reports")
    );
    for report in &page.reports {
        let (tone, word) = if report.kind == "security" {
            ("tone-bad", text(report.event.as_ref()))
        } else {
            (
                "tone-info",
                format!("Firmware: {}", text(report.status.as_ref())),
            )
        };
        let _ = write!(
            out,
            "<li><span class=\"badge {tone}\">{word}</span> {at}<span class=\"said muted\">{detail}</span></li>",
            at = ctx.when(report.at),
            detail = report.detail.as_deref().map(escape).unwrap_or_default(),
        );
    }
    out.push_str("</ol></section>");
    out
}

/// A new password, or retirement — each with a reason (spec 021).
fn manage(page: &StationPage, ctx: &Ctx<'_>) -> String {
    let lang = ctx.lang;
    let action = |path: &str, id: &str, label: &str, button: &str, tone: &str| {
        format!(
            "<form method=\"post\" action=\"{href}/{path}\"><p><label for=\"{id}\">{label}</label><br>\
             <input id=\"{id}\" name=\"reason\" required minlength=\"10\" type=\"text\"></p>\
             <p><button class=\"btn {tone}\" type=\"submit\">{button}</button></p></form>",
            href = escape(&href(&page.identity)),
        )
    };
    format!(
        "<section class=\"card noprint\" aria-labelledby=\"manage-h\"><h2 id=\"manage-h\">{title}</h2>\
         <p><a class=\"btn\" href=\"{href}/edit\">{change}</a></p>{password}{retire}</section>",
        title = lang.pick("Verwalten", "Manage"),
        href = escape(&href(&page.identity)),
        change = lang.pick(
            "Tarif, Leistung, Standort ändern",
            "Change tariff, power, location"
        ),
        password = action(
            "password",
            "password-reason",
            lang.pick("Neues Passwort — warum?", "New password — why?"),
            lang.pick("Neues Passwort erzeugen", "Mint a new password"),
            "",
        ),
        retire = action(
            "retire",
            "retire-reason",
            lang.pick(
                "Außer Betrieb nehmen — warum? Die Station wird danach abgewiesen.",
                "Retire — why? The station is refused afterwards.",
            ),
            lang.pick("Außer Betrieb nehmen", "Retire"),
            "danger",
        ),
    )
}

/// Its last sessions, with their receipts.
fn recent(page: &StationPage, ctx: &Ctx<'_>, receipts: &str) -> String {
    let lang = ctx.lang;
    let mut out = format!(
        "<section class=\"card\" aria-labelledby=\"recent-h\"><h2 id=\"recent-h\">{}</h2>",
        lang.pick("Letzte Ladevorgänge", "Last sessions")
    );
    if page.recent.is_empty() {
        let _ = write!(
            out,
            "<p class=\"empty\">{}</p>",
            lang.pick("Noch kein Ladevorgang.", "No session yet.")
        );
    } else {
        out.push_str("<ol class=\"values\">");
        for session in &page.recent {
            let badge = if session.billed {
                format!(
                    "<span class=\"badge tone-good\">✓ {}</span>",
                    lang.pick("abgerechnet", "billed")
                )
            } else {
                format!(
                    "<span class=\"badge tone-bad\">✕ {}</span>",
                    lang.pick("abgelehnt", "refused")
                )
            };
            let receipt = session
                .receipt
                .as_deref()
                .filter(|_| !receipts.is_empty())
                .map_or_else(String::new, |path| {
                    format!(
                        " <a href=\"{}{}\">{}</a>",
                        escape(receipts.trim_end_matches('/')),
                        escape(path),
                        lang.pick("Beleg", "Receipt")
                    )
                });
            let _ = write!(
                out,
                "<li>{badge} <span class=\"what\">{tx}</span> {at}{receipt}</li>",
                tx = escape(&session.transaction),
                at = ctx.when(session.at),
            );
        }
        out.push_str("</ol>");
    }
    out.push_str("</section>");
    out
}

/// The link to a station's page, built from its identity alone: every byte
/// outside `[A-Za-z0-9._~-]` is percent-encoded, so an identity cannot change
/// the path (SR-095's rule, carried over).
#[must_use]
pub fn href(identity: &str) -> String {
    let mut out = String::from("/station/");
    for byte in identity.bytes() {
        if byte.is_ascii_alphanumeric() || b"._~-".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> StationPage {
        serde_json::from_value(serde_json::json!({
            "identity": "CP-1", "evse": "DE*ABC*E00001", "ocpp": "2.1", "security_profile": 2,
            "rated_power_kw": "150", "commissioned": "2024-05-02",
            "location": {"id": "L", "name": "Markt", "street": "Ring", "house_number": "7",
                "postal_code": "52062", "city": "Aachen", "country": "DE",
                "latitude": "50.775346", "longitude": "6.083887", "time_zone": "Europe/Berlin"},
            "presence": {"state": "online", "since": "2026-11-20T07:00:00Z"},
            "reported_identity": {"vendor": "<script>alert(1)</script>", "model": "M", "at": "2026-11-20T07:00:00Z"},
            "firmware_history": [{"at": "2026-11-20T07:00:00Z", "component": "station", "from": "1.2", "to": "1.3"}],
            "meters": [], "boots_7d": 1,
            "reports": [{"at": "2026-11-20T08:00:00Z", "kind": "security", "event": "TamperDetectionActivated", "detail": "lid"}],
            "success": {"today": null, "week": null}, "recent": []
        }))
        .unwrap()
    }

    #[test]
    fn sr_137_what_a_station_says_is_shown_and_never_run() {
        let html = render(
            &page(),
            Lang::De,
            &TimeZone::new("Europe/Berlin").unwrap(),
            time::macros::datetime!(2026-11-20 12:00 UTC),
            "https://estate.example",
        );
        assert!(!html.contains("<script>alert"), "escaped");
        assert!(html.contains("&lt;script&gt;"), "and still visible");
        assert!(html.contains("1.2 → 1.3"));
        assert!(html.contains("MessEG §31(2) Nr. 4"));
        // The place, with a map the page itself never loads.
        assert!(html.contains("https://www.openstreetmap.org/?mlat=50.775346&amp;mlon=6.083887"));
        assert!(html.contains("geo:50.775346,6.083887"));
        // SR-140: a station's security report is on its page.
        assert!(
            html.contains("TamperDetectionActivated"),
            "the report is listed"
        );
    }

    #[test]
    fn an_identity_cannot_leave_the_station_path() {
        assert_eq!(href("CP-1"), "/station/CP-1");
        assert_eq!(href("../a b"), "/station/..%2Fa%20b");
        assert_eq!(href("DE*X"), "/station/DE%2AX");
    }
}
