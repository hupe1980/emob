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
    /// Operators' notes, newest first.
    #[serde(default)]
    pub notes: Vec<Note>,
    /// The reservation holding it (spec 025).
    #[serde(default)]
    pub reservation: Option<Held>,
    /// The price in force (spec 015).
    #[serde(default)]
    pub price: Option<Price>,
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

/// The price in force, as the price page shows it.
#[derive(Debug, Clone, Deserialize)]
pub struct Price {
    /// `0,49 € pro kWh`.
    pub line: String,
    /// The Preisstand.
    pub preisstand: String,
    /// `/p/{evse}` on the public shell.
    pub path: String,
}

/// A reservation holding the point.
#[derive(Debug, Clone, Deserialize)]
pub struct Held {
    /// Its number on the station.
    pub number: u64,
    /// The card's tail, `…F6` — never its UID.
    pub card: String,
    /// Until.
    #[serde(with = "time::serde::rfc3339")]
    pub until: time::OffsetDateTime,
    /// Who asked, as an actor.
    pub by: String,
}

/// An operator's note.
#[derive(Debug, Clone, Deserialize)]
pub struct Note {
    /// Its number on the station.
    pub number: u64,
    /// Who wrote it, as an actor: `operator:<name>`.
    pub by: String,
    /// What it says.
    pub text: String,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: time::OffsetDateTime,
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
    // Whether the point is held, then what the next person needs to know,
    // before the numbers.
    body.push_str(&reservation(page, &ctx));
    body.push_str(&notes(page, &ctx));
    body.push_str(&rate(page, &ctx));
    body.push_str(&asset(page, &ctx, receipts));
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

/// The reservation holding the point, cancellable, or a form to hold it
/// for a card (spec 025). The card shows as its tail only (SR-176).
fn reservation(page: &StationPage, ctx: &Ctx<'_>) -> String {
    let lang = ctx.lang;
    let station = escape(&href(&page.identity));
    let mut out = format!(
        "<section class=\"card\" id=\"reservation\" aria-labelledby=\"reservation-h\"><h2 id=\"reservation-h\">{}</h2>",
        lang.pick("Reservierung", "Reservation")
    );
    if let Some(held) = &page.reservation {
        let _ = write!(
            out,
            "<p class=\"why tone-warn\"><strong>{held_until} {until}</strong> · {left} · {for_card} <code>{card}</code> · {by}</p>\
             <form method=\"post\" action=\"{station}/reservations/{number}/cancel\" class=\"noprint\">\
             <button class=\"btn ghost\" type=\"submit\">{cancel}</button></form></section>",
            held_until = lang.pick("Reserviert bis", "Held until"),
            until = ctx.when(held.until),
            left = left(held.until - ctx.now, lang),
            for_card = lang.pick("für Karte", "for card"),
            card = escape(&held.card),
            by = escape(held.by.strip_prefix("operator:").unwrap_or(&held.by)),
            number = held.number,
            cancel = lang.pick("Reservierung aufheben", "Cancel the reservation"),
        );
        return out;
    }
    let _ = write!(
        out,
        "<details class=\"noprint\"><summary>{add}</summary>{form}</details></section>",
        add = lang.pick("+ Für eine Karte reservieren", "+ Hold for a card"),
        form = reserve_form(&page.identity, None, lang),
    );
    out
}

/// "noch 27 min", "noch 1 h 5 min" — how long a hold has left.
fn left(remaining: time::Duration, lang: Lang) -> String {
    let minutes = remaining.whole_minutes().max(0);
    let span = if minutes >= 60 {
        format!("{} h {} min", minutes / 60, minutes % 60)
    } else {
        format!("{minutes} min")
    };
    match lang {
        Lang::De => format!("noch {span}"),
        Lang::En => format!("{span} left"),
    }
}

/// What the operator typed, put back after a refusal.
pub struct Typed<'a> {
    /// The card's UID.
    pub uid: &'a str,
    /// How long.
    pub minutes: u32,
    /// What for.
    pub reason: &'a str,
}

/// The form that holds a point for a card — on the asset card, and again,
/// filled in, beside a refusal.
#[must_use]
pub fn reserve_form(identity: &str, typed: Option<&Typed<'_>>, lang: Lang) -> String {
    let chosen = typed.map_or(30, |typed| typed.minutes);
    let mut minutes = String::new();
    for m in [15, 30, 60, 120, 240, 480] {
        let _ = write!(
            minutes,
            "<option value=\"{m}\"{selected}>{label}</option>",
            selected = if m == chosen { " selected" } else { "" },
            label = if m >= 60 {
                format!("{} h", m / 60)
            } else {
                format!("{m} min")
            },
        );
    }
    format!(
        "<form method=\"post\" action=\"{station}/reservations\">\
         <p><label for=\"reserve-uid\">{card}</label><br><input type=\"text\" id=\"reserve-uid\" name=\"uid\" required maxlength=\"36\" autocomplete=\"off\" spellcheck=\"false\" inputmode=\"latin\" value=\"{uid}\" placeholder=\"04A1B2C3D4E5F6\" aria-describedby=\"reserve-hint\"></p>\
         <p><label for=\"reserve-minutes\">{how_long}</label><br><select id=\"reserve-minutes\" name=\"minutes\">{minutes}</select></p>\
         <p><label for=\"reserve-reason\">{why}</label><br><input type=\"text\" id=\"reserve-reason\" name=\"reason\" required maxlength=\"200\" value=\"{reason}\" placeholder=\"{example}\"></p>\
         <p class=\"muted\" id=\"reserve-hint\">{hint}</p><p><button class=\"btn\" type=\"submit\">{save}</button></p></form>",
        station = escape(&href(identity)),
        uid = escape(typed.map_or("", |typed| typed.uid)),
        reason = escape(typed.map_or("", |typed| typed.reason)),
        card = lang.pick("Karte (UID, wie die Station sie liest)", "Card (UID, as the station reads it)"),
        how_long = lang.pick("Wie lange", "For how long"),
        why = lang.pick("Wofür", "What for"),
        example = lang.pick("Flottenfahrzeug kommt um neun", "Fleet car due at nine"),
        hint = lang.pick(
            "Nur diese Karte lädt dann hier. Der Anbieter muss die Karte kennen, die Station muss zustimmen; die Preisseite zeigt nur, bis wann.",
            "Only this card charges here meanwhile. The provider must know the card and the station must agree; the price page shows only until when.",
        ),
        save = lang.pick("Reservieren", "Hold the point"),
    )
}

/// A refusal of a reservation in the operator's words: the station's own
/// answer, the provider's, or the rule — whatever `csmsd` said.
#[must_use]
pub fn refusal_text(reason: &str, lang: Lang) -> String {
    let station = [
        (
            "answered Occupied",
            "Die Station ist gerade belegt.",
            "The station is in use right now.",
        ),
        (
            "answered Faulted",
            "Die Station meldet eine Störung.",
            "The station reports a fault.",
        ),
        (
            "answered Unavailable",
            "Die Station ist außer Betrieb.",
            "The station is out of service.",
        ),
        (
            "answered Rejected",
            "Die Station nimmt keine Reservierungen an.",
            "The station does not take reservations.",
        ),
        (
            "not connected",
            "Die Station ist gerade nicht verbunden.",
            "The station is not connected right now.",
        ),
        (
            "no answer",
            "Die Station hat nicht geantwortet.",
            "The station did not answer.",
        ),
        (
            "CALLERROR",
            "Die Station kennt keine Reservierungen.",
            "The station does not support reservations.",
        ),
        (
            "answers Blocked",
            "Die Karte ist gesperrt.",
            "The card is blocked.",
        ),
        (
            "answers Expired",
            "Der Vertrag der Karte ist abgelaufen.",
            "The card's contract has ended.",
        ),
        (
            "answers Invalid",
            "Der Anbieter kennt die Karte nicht.",
            "The provider does not know the card.",
        ),
        (
            "5 to 1440 minutes",
            "Eine Reservierung dauert 5 Minuten bis einen Tag.",
            "A reservation lasts 5 minutes to a day.",
        ),
    ];
    if let Some((_, de, en)) = station.iter().find(|(said, _, _)| reason.contains(said)) {
        return lang.pick(de, en).to_owned();
    }
    if let Some(at) = reason.find("is held until ") {
        let until = reason[at + 14..].split(';').next().unwrap_or_default();
        return match lang {
            Lang::De => format!("Sie ist schon bis {until} reserviert — erst diese aufheben."),
            Lang::En => format!("It is already held until {until} — cancel that first."),
        };
    }
    reason.to_owned()
}

/// The operators' notes, newest first, each removable, and a form for the
/// next one (spec 023). Every text is escaped (SR-155).
fn notes(page: &StationPage, ctx: &Ctx<'_>) -> String {
    let lang = ctx.lang;
    let station = escape(&href(&page.identity));
    let mut out = format!(
        "<section class=\"card\" id=\"notes\" aria-labelledby=\"notes-h\"><h2 id=\"notes-h\">{}</h2>",
        lang.pick("Notizen", "Notes")
    );
    if !page.notes.is_empty() {
        out.push_str("<ol class=\"notes\">");
        for note in &page.notes {
            let _ = write!(
                out,
                "<li><p class=\"text\">{text}</p><span class=\"muted\">{by} · {at}</span>\
                 <form method=\"post\" action=\"{station}/notes/{number}/remove\" class=\"noprint\">\
                 <button class=\"btn ghost\" type=\"submit\" aria-label=\"{remove_label}\">{remove}</button></form></li>",
                text = escape(&note.text),
                by = escape(note.by.strip_prefix("operator:").unwrap_or(&note.by)),
                at = ctx.when(note.at),
                number = note.number,
                remove = lang.pick("Entfernen", "Remove"),
                remove_label = escape(&match lang {
                    Lang::De => format!("Notiz vom {} entfernen", ctx.when(note.at)),
                    Lang::En => format!("Remove the note of {}", ctx.when(note.at)),
                }),
            );
        }
        out.push_str("</ol>");
    }
    let _ = write!(
        out,
        "<details class=\"noprint\"{open}><summary>{add}</summary>\
         <form method=\"post\" action=\"{station}/notes\"><p><label for=\"note-text\">{label}</label><br>\
         <textarea id=\"note-text\" name=\"text\" required maxlength=\"500\" aria-describedby=\"note-hint\" placeholder=\"{example}\"></textarea></p>\
         <p class=\"muted\" id=\"note-hint\">{hint}</p><p><button class=\"btn\" type=\"submit\">{save}</button></p></form></details></section>",
        // Open while there is nothing to read; a disclosure once there is.
        open = if page.notes.is_empty() { " open" } else { "" },
        add = lang.pick("+ Notiz hinzufügen", "+ Add a note"),
        label = lang.pick("Neue Notiz", "New note"),
        example = lang.pick("Zugang über Tor 3, Schlüssel beim Pförtner", "Access through gate 3, key at the gatehouse"),
        hint = lang.pick(
            "Was die Nächste wissen muss — Zugang, Schlüssel, Eigenheiten. Keine Namen oder Nummern von Privatpersonen.",
            "What the next person needs to know — access, keys, quirks. No private persons' names or numbers.",
        ),
        save = lang.pick("Notiz speichern", "Save the note"),
    );
    out
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
fn asset(page: &StationPage, ctx: &Ctx<'_>, public: &str) -> String {
    let lang = ctx.lang;
    let row = |dt: &str, dd: String| format!("<dt>{dt}</dt><dd>{dd}</dd>");
    let mut card = String::new();
    card.push_str(&row("EVSE", escape(&page.evse)));
    card.push_str(&row(
        lang.pick("Leistung", "Power"),
        format!("{} kW", escape(&page.rated_power_kw)),
    ));
    if let Some(price) = &page.price {
        // The public page lives on the public shell; the card says where by
        // its path and names the code a driver compares (spec 015).
        card.push_str(&row(
            lang.pick("Preis", "Price"),
            format!(
                "{line} <span class=\"muted\">· Preisstand <strong>{code}</strong> · \
                 <a href=\"{href}\">{page_word}</a></span>",
                line = escape(&price.line),
                code = escape(&price.preisstand),
                href = escape(&format!("{}{}", public.trim_end_matches('/'), price.path)),
                page_word = lang.pick("Preisseite", "Price page"),
            ),
        ));
    }
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
            "success": {"today": null, "week": null}, "recent": [],
            "notes": [{"number": 2, "by": "operator:anna", "text": "Tor 3\n<i>Schlüssel</i> beim Pförtner", "at": "2026-11-20T09:00:00Z"}]
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
    fn sr_155_a_note_is_text_names_its_author_and_can_be_removed() {
        let html = render(
            &page(),
            Lang::De,
            &TimeZone::new("Europe/Berlin").unwrap(),
            time::macros::datetime!(2026-11-20 12:00 UTC),
            "",
        );
        assert!(html.contains("&lt;i&gt;Schlüssel&lt;/i&gt;"), "{html}");
        assert!(!html.contains("<i>Schlüssel</i>"));
        assert!(
            html.contains("anna ·"),
            "the author, without the actor's prefix"
        );
        assert!(html.contains("action=\"/station/CP-1/notes/2/remove\""));
        assert!(html.contains("action=\"/station/CP-1/notes\""));
        // Before the numbers: what the next person needs comes first.
        assert!(html.find("id=\"notes\"") < html.find("Ladeerfolg"));
    }

    #[test]
    fn a_held_point_says_until_when_for_which_card_and_can_be_cancelled() {
        let ber = TimeZone::new("Europe/Berlin").unwrap();
        let noon = time::macros::datetime!(2026-11-20 12:00 UTC);
        let free = render(&page(), Lang::De, &ber, noon, "");
        assert!(
            free.contains("action=\"/station/CP-1/reservations\""),
            "a form when free"
        );
        let mut held = page();
        held.reservation = Some(Held {
            number: 3,
            card: "…F6".to_owned(),
            until: time::macros::datetime!(2026-11-20 13:30 UTC),
            by: "operator:anna".to_owned(),
        });
        let html = render(&held, Lang::De, &ber, noon, "");
        assert!(
            html.contains(
                ">14:30</time></strong> · noch 1 h 30 min · für Karte <code>…F6</code> · anna"
            ),
            "{html}"
        );
        assert!(html.contains("action=\"/station/CP-1/reservations/3/cancel\""));
        assert!(
            !html.contains("action=\"/station/CP-1/reservations\""),
            "no second hold"
        );
        assert!(html.find("id=\"reservation\"") < html.find("id=\"notes\""));
    }

    #[test]
    fn a_refused_reservation_says_why_in_the_operators_words_and_keeps_what_was_typed() {
        assert_eq!(
            refusal_text("CP-1 answered Occupied", Lang::De),
            "Die Station ist gerade belegt."
        );
        assert_eq!(
            refusal_text(
                "the provider answers Blocked: the token is blocked",
                Lang::En
            ),
            "The card is blocked."
        );
        assert!(
            refusal_text("CP-1 is held until 14:30; cancel that first", Lang::De)
                .contains("bis 14:30")
        );
        let form = reserve_form(
            "CP-1",
            Some(&Typed {
                uid: "04A1<b>",
                minutes: 120,
                reason: "Gast",
            }),
            Lang::De,
        );
        assert!(
            form.contains("value=\"04A1&lt;b&gt;\"") && form.contains("value=\"120\" selected")
        );
        assert!(form.contains("value=\"Gast\""));
    }

    #[test]
    fn swapped_coordinates_are_named_on_the_card_with_a_way_to_fix_them() {
        let mut swapped = page();
        if let Some(place) = &mut swapped.location {
            core::mem::swap(&mut place.latitude, &mut place.longitude);
        }
        let html = render(
            &swapped,
            Lang::De,
            &TimeZone::new("Europe/Berlin").unwrap(),
            time::macros::datetime!(2026-11-20 12:00 UTC),
            "",
        );
        assert!(
            html.contains("Breite und Länge sind vermutlich vertauscht"),
            "{html}"
        );
        assert!(html.contains("href=\"/station/CP-1/edit\""));
        assert!(
            !render(
                &page(),
                Lang::De,
                &TimeZone::new("Europe/Berlin").unwrap(),
                time::macros::datetime!(2026-11-20 12:00 UTC),
                ""
            )
            .contains("vertauscht")
        );
    }

    #[test]
    fn an_identity_cannot_leave_the_station_path() {
        assert_eq!(href("CP-1"), "/station/CP-1");
        assert_eq!(href("../a b"), "/station/..%2Fa%20b");
        assert_eq!(href("DE*X"), "/station/DE%2AX");
    }
}
