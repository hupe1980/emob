//! Stations: find one among ten thousand, and onboard a new one (spec 021).
//!
//! `/stations` searches `csmsd`'s registry — identity, EVSE, vendor, model,
//! serial, firmware — in pages. `/stations/new` is the onboarding form; its
//! answer is an **installer sheet**: where the station connects, as whom, and
//! its password, shown this once and printable. Every write is forwarded with
//! the operator's own token, so `csmsd` journals the operator; and a write is
//! accepted only from this origin (SR-148).

use core::fmt::Write as _;

use axum::http::HeaderMap;
use emob_service::ui::{self, Lang, escape};
use serde::Deserialize;

/// One row of `csmsd`'s search.
#[derive(Debug, Clone, Deserialize)]
pub struct Row {
    /// The identity.
    pub identity: String,
    /// The point.
    pub evse: String,
    /// The OCPP version.
    pub ocpp: String,
    /// `online`, `offline`, `unknown`.
    pub presence: String,
    /// What it said at boot.
    pub vendor: Option<String>,
    /// Its model.
    pub model: Option<String>,
    /// Its firmware.
    pub firmware: Option<String>,
    /// Its mobile network, MCC-MNC.
    #[serde(default)]
    pub network: Option<String>,
    /// Who runs it.
    #[serde(default)]
    pub carrier: Option<String>,
    /// `14:30` while it is held for a driver.
    #[serde(default)]
    pub reserved_until: Option<String>,
}

/// `csmsd`'s map of a search (spec 022): dots, or a grid.
#[derive(Debug, Clone, Deserialize)]
pub struct Drawn {
    /// The stations, when few enough to draw one by one.
    #[serde(default)]
    pub points: Vec<DrawnPoint>,
    /// Otherwise the occupied cells.
    #[serde(default)]
    pub cells: Vec<DrawnCell>,
}

/// One station on the map.
#[derive(Debug, Clone, Deserialize)]
pub struct DrawnPoint {
    /// Its identity.
    pub identity: String,
    /// Degrees north.
    pub latitude: String,
    /// Degrees east.
    pub longitude: String,
    /// `online`, `offline`, `unknown`.
    pub presence: String,
}

/// One cell.
#[derive(Debug, Clone, Deserialize)]
pub struct DrawnCell {
    /// Edges, degrees.
    pub west: String,
    /// Edges, degrees.
    pub south: String,
    /// Edges, degrees.
    pub east: String,
    /// Edges, degrees.
    pub north: String,
    /// Stations inside.
    pub stations: u64,
    /// Of them, silent.
    pub silent: u64,
}

impl Drawn {
    /// The map as SVG; empty when nothing has coordinates to draw.
    #[must_use]
    pub fn svg(&self, query: &str, lang: Lang) -> String {
        use crate::map::{Cell, Dot, hundredths};
        let dots: Vec<Dot> = self
            .points
            .iter()
            .filter_map(|point| {
                Some(Dot {
                    identity: point.identity.clone(),
                    lon: hundredths(&point.longitude)?,
                    lat: hundredths(&point.latitude)?,
                    presence: point.presence.clone(),
                })
            })
            .collect();
        let cells: Vec<Cell> = self
            .cells
            .iter()
            .filter_map(|cell| {
                Some(Cell {
                    area: format!("{},{},{},{}", cell.west, cell.south, cell.east, cell.north),
                    bounds: (
                        hundredths(&cell.west)?,
                        hundredths(&cell.south)?,
                        hundredths(&cell.east)?,
                        hundredths(&cell.north)?,
                    ),
                    stations: cell.stations,
                    silent: cell.silent,
                })
            })
            .collect();
        crate::map::fleet(&dots, &cells, query, lang)
    }
}

/// What the Stations page shows.
#[derive(Debug)]
pub struct Listing<'a> {
    /// This page of the search.
    pub found: &'a Found,
    /// The map, already drawn.
    pub map: String,
    /// The words searched for.
    pub query: &'a str,
    /// The area searched in.
    pub area: Option<&'a str>,
    /// Where this page starts.
    pub offset: u64,
    /// Rows per page.
    pub limit: usize,
}

/// A page of rows.
#[derive(Debug, Clone, Deserialize)]
pub struct Found {
    /// All matches.
    pub total: u64,
    /// This page.
    pub items: Vec<Row>,
}

/// The onboarding form, as the browser sends it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Form {
    /// OCPP identity.
    #[serde(default)]
    pub identity: String,
    /// `1.6`, `2.0.1`, `2.1`.
    #[serde(default)]
    pub ocpp: String,
    /// `1`, `2`, `3`.
    #[serde(default)]
    pub security_profile: String,
    /// EVSE id.
    #[serde(default)]
    pub evse: String,
    /// kW.
    #[serde(default)]
    pub rated_power_kw: String,
    /// Connector.
    #[serde(default)]
    pub connector: String,
    /// Tariff id.
    #[serde(default)]
    pub tariff: String,
    /// Location id.
    #[serde(default)]
    pub location_id: String,
    /// Location name.
    #[serde(default)]
    pub location_name: String,
    /// Street.
    #[serde(default)]
    pub street: String,
    /// House number.
    #[serde(default)]
    pub house_number: String,
    /// Postal code.
    #[serde(default)]
    pub postal_code: String,
    /// City.
    #[serde(default)]
    pub city: String,
    /// ISO 3166-1 alpha-2.
    #[serde(default)]
    pub country: String,
    /// Latitude.
    #[serde(default)]
    pub latitude: String,
    /// Longitude.
    #[serde(default)]
    pub longitude: String,
    /// IANA zone.
    #[serde(default)]
    pub time_zone: String,
    /// In service since.
    #[serde(default)]
    pub commissioned: String,
    /// How long the display shows a receipt's code, in minutes.
    #[serde(default)]
    pub receipt_display_minutes: String,
    /// Why, for a change.
    #[serde(default)]
    pub reason: String,
}

impl Form {
    /// The form's defaults: what most stations of a German operator are.
    #[must_use]
    pub fn blank() -> Self {
        Self {
            ocpp: "2.0.1".to_owned(),
            security_profile: "2".to_owned(),
            connector: "iec62196_t2_combo".to_owned(),
            country: "DE".to_owned(),
            time_zone: "Europe/Berlin".to_owned(),
            ..Self::default()
        }
    }

    /// The station `csmsd` registers.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let mut station = serde_json::json!({
            "identity": self.identity.trim(),
            "security_profile": self.security_profile.trim().parse::<u8>().unwrap_or(0),
            "ocpp": self.ocpp.trim(),
            "evse": self.evse.trim(),
            "rated_power_kw": self.rated_power_kw.trim().replace(',', "."),
            "tariff": self.tariff.trim(),
            "connector": self.connector.trim(),
            "location": {
                "id": if self.location_id.trim().is_empty() { self.identity.trim() } else { self.location_id.trim() },
                "name": self.location_name.trim(),
                "street": self.street.trim(),
                "house_number": self.house_number.trim(),
                "postal_code": self.postal_code.trim(),
                "city": self.city.trim(),
                "country": self.country.trim(),
                "latitude": self.latitude.trim().replace(',', "."),
                "longitude": self.longitude.trim().replace(',', "."),
                "time_zone": self.time_zone.trim(),
            }
        });
        if !self.commissioned.trim().is_empty() {
            station["commissioned"] = self.commissioned.trim().into();
        }
        if let Ok(minutes) = self.receipt_display_minutes.trim().parse::<u8>() {
            station["receipt_display_minutes"] = minutes.into();
        }
        station
    }

    /// The form for changing a station, filled with what it is now.
    #[must_use]
    pub fn of(page: &crate::station::StationPage) -> Self {
        let place = page.location.clone().unwrap_or_default();
        Self {
            identity: page.identity.clone(),
            ocpp: page.ocpp.clone(),
            security_profile: page.security_profile.to_string(),
            evse: page.evse.clone(),
            rated_power_kw: page.rated_power_kw.clone(),
            connector: page.connector.clone(),
            tariff: page.tariff.clone(),
            location_id: place.id,
            location_name: place.name,
            street: place.street,
            house_number: place.house_number,
            postal_code: place.postal_code,
            city: place.city,
            country: place.country,
            latitude: place.latitude,
            longitude: place.longitude,
            time_zone: place.time_zone,
            commissioned: page.commissioned.clone().unwrap_or_default(),
            receipt_display_minutes: page
                .receipt_display_minutes
                .map(|m| m.to_string())
                .unwrap_or_default(),
            reason: String::new(),
        }
    }
}

/// Whether the form registers a station or changes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose<'a> {
    /// `/stations/new`.
    Register,
    /// `/station/{identity}/edit`.
    Change(&'a str),
}

/// The operator's own token: the password of the Basic credentials the
/// browser sent, which `ui::operator_page` has already admitted.
#[must_use]
pub fn operator_token(headers: &HeaderMap) -> Option<String> {
    use base64::Engine as _;
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let encoded = value.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    text.split_once(':')
        .map(|(_, password)| password.to_owned())
}

/// Whether a write came from this page (SR-148): `Sec-Fetch-Site:
/// same-origin`, or, from a browser that does not send it, an `Origin` naming
/// this host.
#[must_use]
pub fn same_origin(headers: &HeaderMap) -> bool {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    match header("sec-fetch-site") {
        Some(site) => site == "same-origin",
        None => match (header("origin"), header("host")) {
            (Some(origin), Some(host)) => {
                origin == format!("http://{host}") || origin == format!("https://{host}")
            }
            _ => false,
        },
    }
}

/// `/stations`: the search and the way to add one.
#[must_use]
pub fn render_list(listing: &Listing<'_>, lang: Lang) -> String {
    let Listing {
        found,
        query,
        area,
        offset,
        limit,
        ..
    } = *listing;
    let area_input = area.map_or_else(String::new, |area| {
        format!(
            "<input type=\"hidden\" name=\"area\" value=\"{}\">",
            escape(area)
        )
    });
    let mut body = format!(
        "<p class=\"eyebrow back\"><a href=\"/\">{back}</a></p>\
         <div class=\"head\"><h1>{title} <span class=\"count\">{total}</span></h1>\
         <a class=\"btn\" href=\"/stations/new\">{add}</a></div>\
         <form class=\"search\" method=\"get\" action=\"/stations\" role=\"search\">\
         <label for=\"q\">{label}</label> <input id=\"q\" name=\"q\" type=\"search\" value=\"{query}\" \
         placeholder=\"{hint}\" autocomplete=\"off\">{area_input} <button class=\"btn\" type=\"submit\">{go}</button></form>",
        back = lang.pick("← Heute", "← Today"),
        title = lang.pick("Stationen", "Stations"),
        total = found.total,
        add = lang.pick("+ Station hinzufügen", "+ Add a station"),
        label = lang.pick("Suchen", "Search"),
        hint = lang.pick(
            "Kennung, EVSE, Modell, Firmware, Netz …",
            "identity, EVSE, model, firmware, network …"
        ),
        go = lang.pick("Suchen", "Search"),
        query = escape(query),
    );
    if let Some(area) = area {
        let _ = write!(
            body,
            "<p class=\"chips\"><span class=\"badge tone-info\">{inside} {area}</span> \
             <a href=\"/stations?q={q}\">{all}</a></p>",
            inside = lang.pick("Im Gebiet", "In the area"),
            area = escape(area),
            q = escape(&query.replace(' ', "+")),
            all = lang.pick("Ganze Flotte zeigen", "Show the whole fleet"),
        );
    }
    body.push_str(&listing.map);
    if found.items.is_empty() {
        let _ = write!(
            body,
            "<p class=\"empty\">{}</p>",
            if query.is_empty() {
                lang.pick(
                    "Noch keine Station. „Station hinzufügen“ legt die erste an.",
                    "No station yet. “Add a station” creates the first.",
                )
            } else {
                lang.pick(
                    "Keine Station passt zu dieser Suche.",
                    "No station matches this search.",
                )
            }
        );
    } else {
        body.push_str(&table(&found.items, lang));
        let next = offset + limit as u64;
        if next < found.total {
            let _ = write!(
                body,
                "<p class=\"more\"><a href=\"/stations?q={q}{area}&amp;offset={next}\">{more}</a></p>",
                q = escape(&query.replace(' ', "+")),
                area = area.map_or_else(String::new, |area| format!("&amp;area={}", escape(area))),
                more = lang.pick("Weitere anzeigen", "Show more"),
            );
        }
    }
    ui::page(
        &ui::Shell {
            title: lang.pick("Stationen", "Stations"),
            place: lang.pick("Betrieb", "Operations"),
            lang,
            reading: false,
            script: None,
        },
        &body,
    )
}

/// The rows of a search, one link per station — the keyboard's way to what
/// the map shows.
fn table(rows: &[Row], lang: Lang) -> String {
    let mut body = String::new();
    body.push_str("<div class=\"scroll\"><table><thead><tr>");
    for head in [
        lang.pick("Station", "Station"),
        "EVSE",
        "OCPP",
        lang.pick("Zustand", "State"),
        lang.pick("Modell", "Model"),
        "Firmware",
        lang.pick("Netz", "Network"),
    ] {
        let _ = write!(body, "<th>{head}</th>");
    }
    body.push_str("</tr></thead><tbody>");
    for row in rows {
        let (tone, word) = match row.presence.as_str() {
            "online" => ("tone-good", lang.pick("online", "online")),
            "offline" => ("tone-warn", lang.pick("still", "silent")),
            _ => ("tone-info", lang.pick("nie gehört", "never heard")),
        };
        let _ = write!(
            body,
            "<tr><td><a href=\"{href}\"><strong>{identity}</strong></a></td><td>{evse}</td><td>{ocpp}</td>\
             <td><span class=\"badge {tone}\">{word}</span>{held}</td><td>{model}</td><td>{firmware}</td><td>{network}</td></tr>",
            href = escape(&crate::station::href(&row.identity)),
            identity = escape(&row.identity),
            evse = escape(&row.evse),
            ocpp = escape(&row.ocpp),
            held = row
                .reserved_until
                .as_deref()
                .map_or_else(String::new, |until| format!(
                    " <span class=\"badge tone-info\">{} {}</span>",
                    lang.pick("reserviert bis", "held until"),
                    escape(until)
                )),
            model = escape(&format!(
                "{} {}",
                row.vendor.as_deref().unwrap_or(""),
                row.model.as_deref().unwrap_or("")
            )),
            firmware = escape(row.firmware.as_deref().unwrap_or("—")),
            network = escape(
                row.carrier
                    .as_deref()
                    .or(row.network.as_deref())
                    .unwrap_or("—")
            ),
        );
    }
    body.push_str("</tbody></table></div>");
    body
}

/// The inputs a refusal can be shown at, by the name `csmsd` refuses under.
const INPUTS: &[&str] = &[
    "identity",
    "evse",
    "rated_power_kw",
    "tariff",
    "commissioned",
    "receipt_display_minutes",
    "location_id",
    "location_name",
    "street",
    "house_number",
    "postal_code",
    "city",
    "country",
    "latitude",
    "longitude",
    "time_zone",
    "reason",
];

/// The input a refused field is shown at — `location.country` at `country`,
/// `location.id` at `location_id` — or `None` when the form has no such input
/// and the refusal is shown above it.
fn input_of(field: &str) -> Option<&'static str> {
    let name = match field.strip_prefix("location.") {
        Some("id") => "location_id",
        Some("name") => "location_name",
        Some(rest) => rest,
        None if field == "location" => "latitude",
        None => field,
    };
    INPUTS.iter().copied().find(|input| *input == name)
}

/// One labelled input, marked and explained when `csmsd` refused it.
fn text_field(
    refusal: Option<(&str, &str)>,
    name: &str,
    label: &str,
    value: &str,
    kind: &str,
    hint: &str,
) -> String {
    let refused = refusal.filter(|(f, _)| input_of(f) == Some(name));
    let (aria, note) = refused.map_or((String::new(), String::new()), |(_, reason)| {
        (
            format!(" aria-invalid=\"true\" aria-describedby=\"{name}-err\""),
            format!(
                "<p class=\"why tone-bad\" id=\"{name}-err\">{}</p>",
                escape(reason)
            ),
        )
    });
    format!(
        "<p><label for=\"{name}\">{label}</label><br><input id=\"{name}\" name=\"{name}\" type=\"{kind}\" \
         value=\"{value}\" placeholder=\"{hint}\"{aria}></p>{note}",
        value = escape(value),
        hint = escape(hint),
    )
}

/// One labelled choice.
fn select(name: &str, label: &str, value: &str, options: &[(&str, &str)]) -> String {
    let mut out = format!(
        "<p><label for=\"{name}\">{label}</label><br><select id=\"{name}\" name=\"{name}\">"
    );
    for (option, text) in options {
        let _ = write!(
            out,
            "<option value=\"{option}\"{selected}>{text}</option>",
            selected = if *option == value { " selected" } else { "" }
        );
    }
    out.push_str("</select></p>");
    out
}

/// `/stations/new`: the form, with a refusal beside its field when `csmsd`
/// refused it.
#[must_use]
#[allow(clippy::too_many_lines)] // One form, field by field, in the order it is filled.
pub fn render_form(
    form: &Form,
    purpose: Purpose<'_>,
    refusal: Option<(&str, &str)>,
    lang: Lang,
) -> String {
    let field = |name: &str, label: &str, value: &str, kind: &str, hint: &str| {
        text_field(refusal, name, label, value, kind, hint)
    };
    let general = refusal
        .filter(|(field, _)| input_of(field).is_none())
        .map_or_else(String::new, |(_, reason)| {
            format!(
                "<div class=\"verdict tone-bad\" role=\"alert\"><p>{}</p></div>",
                escape(reason)
            )
        });
    let (back, title, intro, action, submit) = framing(purpose, lang);
    // An identity is not changed — a new one is a new station.
    let identity = match purpose {
        Purpose::Register => field(
            "identity",
            lang.pick("OCPP-Kennung", "OCPP identity"),
            &form.identity,
            "text",
            "CP-0042",
        ),
        Purpose::Change(identity) => format!(
            "<input type=\"hidden\" name=\"identity\" value=\"{}\">",
            escape(identity)
        ),
    };
    let reason = match purpose {
        Purpose::Register => String::new(),
        Purpose::Change(_) => format!(
            "<h2>{}</h2>{}",
            lang.pick("Warum", "Why"),
            field(
                "reason",
                lang.pick(
                    "Grund (mindestens zehn Zeichen)",
                    "Reason (ten characters or more)"
                ),
                &form.reason,
                "text",
                lang.pick("Koordinaten waren vertauscht", "coordinates were swapped")
            ),
        ),
    };
    let body = format!(
        "<p class=\"eyebrow back\">{back}</p><h1>{title}</h1>\
         <p class=\"muted\">{intro}</p>{general}\
         <form method=\"post\" action=\"{action}\" class=\"card\">\
         <h2>{station}</h2>{identity}{ocpp}{profile}{evse}{power}{connector}{tariff}{commissioned}{minutes}\
         <h2>{place}</h2>{location_id}{location_name}{street}{house}{postal}{city}{country}{lat}{lon}{zone}{reason}\
         <p><button class=\"btn\" type=\"submit\">{submit}</button></p></form>",
        action = escape(&action),
        minutes = field(
            "receipt_display_minutes",
            lang.pick(
                "Beleg-Code am Display (Minuten, leer: aus)",
                "Receipt code on the display (minutes, empty: off)"
            ),
            &form.receipt_display_minutes,
            "number",
            ""
        ),
        station = lang.pick("Station", "Station"),
        place = lang.pick("Standort", "Location"),
        ocpp = select(
            "ocpp",
            "OCPP",
            &form.ocpp,
            &[("1.6", "1.6"), ("2.0.1", "2.0.1"), ("2.1", "2.1")]
        ),
        profile = select(
            "security_profile",
            lang.pick("Sicherheitsprofil", "Security profile"),
            &form.security_profile,
            &[
                ("1", "1 — Passwort, ohne TLS (nur intern)"),
                ("2", "2 — Passwort über TLS"),
                ("3", "3 — Client-Zertifikat"),
            ],
        ),
        evse = field("evse", "EVSE-ID", &form.evse, "text", "DE*ABC*E00042"),
        power = field(
            "rated_power_kw",
            lang.pick("Leistung (kW)", "Power (kW)"),
            &form.rated_power_kw,
            "text",
            "150"
        ),
        connector = select(
            "connector",
            lang.pick("Stecker", "Connector"),
            &form.connector,
            &[
                ("iec62196_t2_combo", "CCS 2 (DC)"),
                ("iec62196_t2", "Typ 2 (AC)"),
                ("chademo", "CHAdeMO"),
                ("iec62196_t1_combo", "CCS 1"),
                ("iec62196_t1", "Typ 1"),
                ("iec62196_t3_a", "Typ 3A"),
                ("domestic_f", "Schuko"),
                ("cee5", "CEE 16 A"),
            ],
        ),
        tariff = field(
            "tariff",
            lang.pick("Tarif-ID", "Tariff id"),
            &form.tariff,
            "text",
            "ad-hoc"
        ),
        commissioned = field(
            "commissioned",
            lang.pick("In Betrieb seit", "In service since"),
            &form.commissioned,
            "date",
            ""
        ),
        location_id = field(
            "location_id",
            lang.pick("Standort-ID", "Location id"),
            &form.location_id,
            "text",
            lang.pick("leer: wie die Kennung", "empty: as the identity")
        ),
        location_name = field(
            "location_name",
            lang.pick("Name", "Name"),
            &form.location_name,
            "text",
            "Marktplatz"
        ),
        street = field(
            "street",
            lang.pick("Straße", "Street"),
            &form.street,
            "text",
            ""
        ),
        house = field(
            "house_number",
            lang.pick("Hausnummer", "Number"),
            &form.house_number,
            "text",
            ""
        ),
        postal = field(
            "postal_code",
            lang.pick("PLZ", "Postal code"),
            &form.postal_code,
            "text",
            ""
        ),
        city = field("city", lang.pick("Ort", "City"), &form.city, "text", ""),
        country = field(
            "country",
            lang.pick("Land (ISO, 2 Buchstaben)", "Country (ISO, 2 letters)"),
            &form.country,
            "text",
            "DE"
        ),
        lat = field(
            "latitude",
            lang.pick("Breite", "Latitude"),
            &form.latitude,
            "text",
            "50.775346"
        ),
        lon = field(
            "longitude",
            lang.pick("Länge", "Longitude"),
            &form.longitude,
            "text",
            "6.083887"
        ),
        zone = field(
            "time_zone",
            lang.pick("Zeitzone", "Time zone"),
            &form.time_zone,
            "text",
            "Europe/Berlin"
        ),
    );
    ui::page(
        &ui::Shell {
            title,
            place: lang.pick("Betrieb", "Operations"),
            lang,
            reading: true,
            script: None,
        },
        &body,
    )
}

/// What the form says it is for, and where it goes: back link, title, intro,
/// action and button.
fn framing(
    purpose: Purpose<'_>,
    lang: Lang,
) -> (String, &'static str, &'static str, String, &'static str) {
    match purpose {
        Purpose::Register => (
            "<a href=\"/stations\">".to_owned() + lang.pick("← Stationen", "← Stations") + "</a>",
            lang.pick("Station hinzufügen", "Add a station"),
            lang.pick(
                "Die Station kann sich sofort verbinden. Ihr Passwort wird erzeugt und genau einmal angezeigt.",
                "The station can connect at once. Its password is minted and shown exactly once.",
            ),
            "/stations".to_owned(),
            lang.pick("Station anlegen", "Create the station"),
        ),
        Purpose::Change(identity) => (
            format!(
                "<a href=\"{}\">← {}</a>",
                escape(&crate::station::href(identity)),
                escape(identity)
            ),
            lang.pick("Station ändern", "Change the station"),
            lang.pick(
                "Die Änderung gilt ab der nächsten Verbindung und steht mit deinem Namen und Grund im Verlauf. Das Passwort bleibt.",
                "The change applies from the next connection and is recorded with your name and reason. The password stays.",
            ),
            format!("{}/edit", crate::station::href(identity)),
            lang.pick("Änderung speichern", "Save the change"),
        ),
    }
}

/// The installer sheet: everything a technician types into the station, the
/// password this once.
#[must_use]
pub fn render_sheet(
    identity: &str,
    password: Option<&str>,
    profile: &str,
    ocpp: &str,
    url: Option<&str>,
    lang: Lang,
) -> String {
    let row = |dt: &str, dd: String| format!("<dt>{dt}</dt><dd>{dd}</dd>");
    let mut sheet = String::new();
    sheet.push_str(&row(
        lang.pick("CSMS-Adresse", "CSMS address"),
        url.map_or_else(
            || {
                format!(
                    "<span class=\"muted\">{}</span>",
                    lang.pick("in [desk] ocpp_url eintragen", "set [desk] ocpp_url")
                )
            },
            |url| format!("<code>{}</code>", escape(url)),
        ),
    ));
    sheet.push_str(&row(
        lang.pick("Kennung", "Identity"),
        format!("<code>{}</code>", escape(identity)),
    ));
    sheet.push_str(&row("OCPP", escape(ocpp)));
    sheet.push_str(&row(
        lang.pick("Sicherheitsprofil", "Security profile"),
        escape(profile),
    ));
    if let Some(password) = password {
        sheet.push_str(&row(
            if ocpp == "1.6" {
                "AuthorizationKey"
            } else {
                lang.pick("Passwort", "Password")
            },
            format!("<p class=\"key\">{}</p>", escape(password)),
        ));
    }
    let body = format!(
        "<div class=\"verdict tone-good\"><span class=\"icon\" aria-hidden=\"true\">✓</span><div>\
         <h1>{done}</h1><p>{connect}</p></div></div>\
         <section class=\"card\" aria-labelledby=\"sheet-h\"><h2 id=\"sheet-h\">{title}</h2><dl class=\"kv\">{sheet}</dl>\
         {warning}</section>\
         <p class=\"noprint\"><a class=\"btn\" href=\"{station}\">{open}</a> <a href=\"/stations/new\">{another}</a></p>",
        done = lang.pick("Station angelegt", "Station created"),
        connect = lang.pick(
            "Sie kann sich jetzt verbinden — ein Neustart ist nicht nötig.",
            "It can connect now — no restart is needed.",
        ),
        title = lang.pick("Datenblatt für die Installation", "Installer sheet"),
        warning = if password.is_some() {
            format!(
                "<p class=\"why tone-warn\">{}</p>",
                lang.pick(
                    "Das Passwort wird nur jetzt angezeigt und nirgends gespeichert. Drucken Sie diese Seite oder übertragen Sie es in die Station.",
                    "The password is shown only now and stored nowhere. Print this page or enter it into the station.",
                )
            )
        } else {
            String::new()
        },
        station = escape(&crate::station::href(identity)),
        open = lang.pick("Zur Station", "Open the station"),
        another = lang.pick("Noch eine Station", "Add another"),
    );
    ui::page(
        &ui::Shell {
            title: lang.pick("Station angelegt", "Station created"),
            place: lang.pick("Betrieb", "Operations"),
            lang,
            reading: true,
            script: None,
        },
        &body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sr_148_a_write_from_another_origin_is_not_same_origin() {
        let mut headers = HeaderMap::new();
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(!same_origin(&headers));
        headers.insert("sec-fetch-site", "same-origin".parse().unwrap());
        assert!(same_origin(&headers));
        let mut old = HeaderMap::new();
        old.insert("host", "desk.example".parse().unwrap());
        old.insert("origin", "https://evil.example".parse().unwrap());
        assert!(!same_origin(&old));
        old.insert("origin", "https://desk.example".parse().unwrap());
        assert!(same_origin(&old));
        assert!(!same_origin(&HeaderMap::new()), "no evidence is no write");
    }

    #[test]
    fn the_sheet_shows_the_password_once_and_says_so() {
        let html = render_sheet(
            "CP-1",
            Some("a1b2"),
            "2",
            "2.0.1",
            Some("wss://csms/ocpp"),
            Lang::De,
        );
        assert!(html.contains("a1b2") && html.contains("nur jetzt"));
        assert!(html.contains("wss://csms/ocpp") && html.contains("Neustart ist nicht nötig"));
    }

    #[test]
    fn a_refused_location_field_is_marked_at_its_own_input() {
        assert_eq!(input_of("location.country"), Some("country"));
        assert_eq!(input_of("location.id"), Some("location_id"));
        assert_eq!(input_of("location.time_zone"), Some("time_zone"));
        assert_eq!(
            input_of("ocpp"),
            None,
            "a select's refusal is shown above the form"
        );
        let html = render_form(
            &Form::blank(),
            Purpose::Register,
            Some((
                "location.country",
                "\"DEU\" is not an ISO 3166-1 alpha-2 code like DE",
            )),
            Lang::En,
        );
        assert!(html.contains("id=\"country\" name=\"country\" type=\"text\" value=\"DE\" placeholder=\"DE\" aria-invalid"));
        assert!(!html.contains("id=\"location_id\" name=\"location_id\" type=\"text\" value=\"\" placeholder=\"empty: as the identity\" aria-invalid"));
    }

    #[test]
    fn a_refused_field_is_marked_where_it_is() {
        let html = render_form(
            &Form::blank(),
            Purpose::Register,
            Some(("tariff", "no tariff has the id")),
            Lang::De,
        );
        assert!(html.contains("aria-invalid=\"true\" aria-describedby=\"tariff-err\""));
    }

    /// Spec 022, SC-001: the Stations page holds the operator budget with
    /// the most the map draws — 300 dots, or the grid a fleet of 10 000
    /// becomes — and a full page of rows.
    #[test]
    fn sc_001_the_stations_page_stays_inside_the_operator_budget() {
        let rows: Vec<Row> = (0..crate::heute::ROWS)
            .map(|n| Row {
                identity: format!("CP-{n:05}"),
                evse: format!("DE*ABC*E{n:05}"),
                ocpp: "2.0.1".into(),
                presence: "online".into(),
                vendor: Some("Alpitronic".into()),
                model: Some("HYC300".into()),
                firmware: Some("1.3.1".into()),
                network: Some("26201".into()),
                carrier: Some("Telekom Deutschland".into()),
                reserved_until: (n == 0).then(|| "14:30".into()),
            })
            .collect();
        let found = Found {
            total: 10_000,
            items: rows,
        };
        let degrees = |h: usize| format!("{}.{:02}", h / 100, h % 100);
        let dots = Drawn {
            points: (0..300)
                .map(|n| DrawnPoint {
                    identity: format!("CP-{n:05}"),
                    latitude: degrees(4730 + n % 17 * 45),
                    longitude: degrees(600 + n / 17 * 50),
                    presence: if n % 7 == 0 { "offline" } else { "online" }.into(),
                })
                .collect(),
            cells: Vec::new(),
        };
        let grid = Drawn {
            points: Vec::new(),
            cells: (0..256)
                .map(|n| DrawnCell {
                    west: degrees(600 + n % 16 * 57),
                    south: degrees(4730 + n / 16 * 49),
                    east: degrees(657 + n % 16 * 57),
                    north: degrees(4779 + n / 16 * 49),
                    stations: 39,
                    silent: (n % 5) as u64,
                })
                .collect(),
        };
        for drawn in [dots, grid] {
            let html = render_list(
                &Listing {
                    found: &found,
                    map: drawn.svg("", Lang::De),
                    query: "",
                    area: None,
                    offset: 0,
                    limit: crate::heute::ROWS,
                },
                Lang::De,
            );
            assert!(html.contains("<svg"), "a map is drawn");
            assert!(
                html.len() < 150_000,
                "the Stations page weighs {} bytes",
                html.len()
            );
        }
    }
}
