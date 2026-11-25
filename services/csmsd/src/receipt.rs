//! `/r/{token}` — the receipt a driver can check (spec 003).
//!
//! `[MessEG §33]` lets an operator bill only off a measured value the customer
//! can verify. This is where that sentence reaches the customer: one page per
//! settled or refused transaction, unauthenticated, opened by a link that
//! arrives on the invoice line. It leads with the verdict, then the energy the
//! signed readings state, the price the CDR carries, where and when — then the
//! meter's key, the S.A.F.E. transparency file, and a button that checks every
//! signed value **on the driver's device** with a WebAssembly build of `ocmf`
//! (`emob-verify`), so nothing on this server has to be trusted.
//!
//! # The page computes nothing that decides
//!
//! The verdict is the journalled one; the figures are the CDR's (`Rated` lines,
//! `tax_summary`, `Cost::gross` — the functions the invoice reads). The
//! transparency file is the journalled records replayed through
//! `Evidence::assemble` at the session's instant — the replay principle III
//! exists for, not a second opinion.
//!
//! # The token (SR-060..SR-063)
//!
//! `base64url(HMAC-SHA256(receipt.key, "emob-receipt\0" ‖ station ‖ "\0" ‖
//! transaction)[..16])` — 128 bits, 22 characters. The key is 32 bytes from the
//! OS CSPRNG in `receipt.key` beside the journal, never in it; the journal holds
//! only `hex(SHA-256(token))`. Deterministic, so the link can be printed again
//! on a later invoice and linked from the desk, as mako 088 FR-003 decided. An
//! unknown, malformed or undecided token is the shell's own 404, and nothing
//! here logs a path.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path as UrlPath, Query, State as Extract};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine as _;
use emob_cdr::Cdr;
use emob_core::{EvseId, TimeZone};
use emob_eichrecht::{Evidence, FindingKind, PublicKey, Record};
use emob_service::journal::Journal;
use emob_service::ui::{self, Lang, Shell, escape};
use emob_tariff::{Dimension, TaxIncluded};
use hmac::{Hmac, KeyInit as _, Mac as _};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive as _;
use sha2::{Digest as _, Sha256};

use crate::Csmsd;
use crate::journal::{Issued, Key, Receipt, Verdict};

/// The receipt key's file, beside the journal.
pub const KEY_FILE: &str = "receipt.key";

/// The verifier, built by `just wasm` and compiled in.
pub const VERIFIER: &[u8] = include_bytes!("../assets/emob_verify.wasm");

/// The forty lines that run it.
pub const SCRIPT: &str = include_str!("receipt.js");

/// Where the verifier and its script are served.
pub const VERIFIER_PATH: &str = "/r/verify.wasm";
/// …and the script.
pub const SCRIPT_PATH: &str = "/r/verify.js";

/// How many requests per second `/r/` takes before it answers 429 (SR-067).
pub const PER_SECOND: u32 = 60;

/// A token is 16 bytes, base64url without padding.
const TOKEN_LEN: usize = 22;

/// The key receipts are minted under, and where their links point.
pub struct Receipts {
    key: [u8; 32],
    public_url: String,
}

impl core::fmt::Debug for Receipts {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Receipts")
            .field("public_url", &self.public_url)
            .finish_non_exhaustive()
    }
}

impl Receipts {
    /// The key in `dir/receipt.key`, created from the OS CSPRNG with mode 0600
    /// the first time (SR-060, SR-062).
    ///
    /// # Errors
    ///
    /// The file cannot be read or created, holds something other than 32
    /// bytes, or the OS has no randomness to give.
    pub fn open(dir: &Path, public_url: &str) -> std::io::Result<Self> {
        let path = dir.join(KEY_FILE);
        let key = match std::fs::read(&path) {
            Ok(bytes) => <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{} does not hold 32 bytes", path.display()),
                )
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let key = fresh()?;
                write_private(&path, &key)?;
                key
            }
            Err(error) => return Err(error),
        };
        Ok(Self::with_key(key, public_url))
    }

    /// A key drawn now and kept nowhere — for a CSMS whose links need not
    /// outlive the process (tests).
    ///
    /// # Errors
    ///
    /// The OS has no randomness to give.
    pub fn ephemeral(public_url: &str) -> std::io::Result<Self> {
        Ok(Self::with_key(fresh()?, public_url))
    }

    fn with_key(key: [u8; 32], public_url: &str) -> Self {
        Self {
            key,
            public_url: public_url.trim_end_matches('/').to_owned(),
        }
    }

    /// The token for one transaction.
    #[must_use]
    pub fn token(&self, key: &Key) -> String {
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&self.key) else {
            unreachable!("HMAC takes a key of any length")
        };
        mac.update(b"emob-receipt\0");
        mac.update(key.0.as_bytes());
        mac.update(b"\0");
        mac.update(key.1.as_bytes());
        let tag = mac.finalize().into_bytes();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&tag[..16])
    }

    /// Where the public pages are served, without a trailing slash.
    #[must_use]
    pub fn public_url(&self) -> &str {
        &self.public_url
    }

    /// `/r/{token}`.
    #[must_use]
    pub fn path(&self, key: &Key) -> String {
        format!("/r/{}", self.token(key))
    }

    /// The absolute link a driver is handed.
    #[must_use]
    pub fn url(&self, key: &Key) -> String {
        format!("{}{}", self.public_url, self.path(key))
    }

    /// The fact that files one transaction's receipt.
    #[must_use]
    pub fn issue(
        &self,
        key: &Key,
        evse: EvseId,
        at: Option<time::OffsetDateTime>,
        signed: Vec<String>,
    ) -> crate::Event {
        crate::Event::Receipted {
            station: key.0.clone(),
            transaction: key.1.clone(),
            receipt: Receipt {
                digest: digest(&self.token(key)),
                evse,
                at,
                signed,
            },
        }
    }
}

fn fresh() -> std::io::Result<[u8; 32]> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(key)
}

fn write_private(path: &Path, key: &[u8; 32]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(key)?;
    file.sync_all()
}

/// `hex(SHA-256(token))` — the only form a token is stored in.
#[must_use]
pub fn digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn well_formed(token: &str) -> bool {
    token.len() == TOKEN_LEN
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

// ── Where a point stands ────────────────────────────────────────────────────

/// What the page says about where a point stands.
#[derive(Debug, Clone)]
pub struct Site {
    /// The location's name.
    pub name: String,
    /// Street, number, postal code, city.
    pub address: String,
    /// The zone its local time is read in.
    pub zone: TimeZone,
}

/// Every provisioned point's site, by EVSE.
#[must_use]
pub fn sites(stations: &[emob_service::estate::Station]) -> BTreeMap<EvseId, Site> {
    stations
        .iter()
        .map(|station| {
            let l = &station.location;
            (
                station.evse.clone(),
                Site {
                    name: l.name.clone(),
                    address: format!(
                        "{} {}, {} {}",
                        l.street, l.house_number, l.postal_code, l.city
                    ),
                    zone: TimeZone::new(&l.time_zone).unwrap_or_else(|_| TimeZone::utc()),
                },
            )
        })
        .collect()
}

// ── The window ──────────────────────────────────────────────────────────────

/// One global fixed window (SR-067). The 128-bit token is what stops
/// enumeration; this bounds what a flood costs a CSMS that also serves stations.
#[derive(Debug)]
pub(crate) struct Window {
    limit: u32,
    held: Mutex<(Instant, u32)>,
}

impl Window {
    pub(crate) fn new(limit: u32) -> Self {
        Self {
            limit,
            held: Mutex::new((Instant::now(), 0)),
        }
    }

    pub(crate) fn admits(&self) -> bool {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if now.duration_since(held.0) >= Duration::from_secs(1) {
            *held = (now, 0);
        }
        held.1 += 1;
        held.1 <= self.limit
    }
}

// ── The routes ──────────────────────────────────────────────────────────────

struct Shared<J: Journal> {
    csmsd: Arc<Csmsd<J>>,
    sites: BTreeMap<EvseId, Site>,
    window: Window,
}

/// The receipt routes and the shared stylesheet, unauthenticated.
pub fn router<J: Journal>(csmsd: Arc<Csmsd<J>>, sites: BTreeMap<EvseId, Site>) -> Router {
    router_limited(csmsd, sites, PER_SECOND)
}

fn router_limited<J: Journal>(
    csmsd: Arc<Csmsd<J>>,
    sites: BTreeMap<EvseId, Site>,
    limit: u32,
) -> Router {
    let shared = Arc::new(Shared {
        csmsd,
        sites,
        window: Window::new(limit),
    });
    Router::new()
        .route(VERIFIER_PATH, get(verifier))
        .route(SCRIPT_PATH, get(script))
        .route("/r/{token}", get(page::<J>))
        .route("/r/{token}/transparenz.xml", get(file::<J>))
        .with_state(shared)
        .merge(ui::assets())
}

#[derive(serde::Deserialize)]
struct LangQuery {
    lang: Option<String>,
}

/// The shell's own answer for a path it does not serve (SR-061).
fn not_found() -> Response {
    // Built the way the router's own fallback builds it — an empty body and no
    // header at all — so not even a `content-length` tells the two apart.
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::NOT_FOUND;
    response
}

fn too_many() -> Response {
    let mut response = StatusCode::TOO_MANY_REQUESTS.into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

/// The headers every receipt answer carries beyond [`ui::hardened`] (SR-066).
fn private(mut response: Response, csp: &'static str) -> Response {
    response = ui::hardened(response, csp);
    response.headers_mut().insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    response
}

/// What one token opens, read out of the fold in one lock.
struct Found {
    token: String,
    issued: Issued,
    verdict: Verdict,
    cdr: Option<Cdr>,
}

fn find<J: Journal>(shared: &Shared<J>, token: &str) -> Option<Found> {
    if !well_formed(token) {
        return None;
    }
    let wanted = digest(token);
    shared.csmsd.with_state(|state| {
        let issued = state.receipts.get(&wanted)?.clone();
        let verdict = issued.verdict.clone()?;
        let cdr = match &verdict {
            Verdict::Billed(key) => Some(state.cdrs.get(key)?.clone()),
            Verdict::Refused { .. } => None,
        };
        Some(Found {
            token: token.to_owned(),
            issued,
            verdict,
            cdr,
        })
    })
}

async fn page<J: Journal>(
    Extract(shared): Extract<Arc<Shared<J>>>,
    UrlPath(token): UrlPath<String>,
    Query(query): Query<LangQuery>,
    headers: HeaderMap,
) -> Response {
    if !shared.window.admits() {
        return too_many();
    }
    let Some(found) = find(&shared, &token) else {
        return not_found();
    };
    let lang = Lang::negotiate(
        query.lang.as_deref(),
        headers
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok()),
    );
    let evidence = evidence(&shared.csmsd, &found.issued.receipt);
    let key = registered_key(&shared.csmsd, &found.issued.receipt);
    // The registry first (spec 021): a station registered after start has a
    // site too; the start-time map stays for a test that has no registry.
    let registered = shared.csmsd.with_registry(|registry, _| {
        registry
            .values()
            .find(|station| station.evse == found.issued.receipt.evse)
            .map(|station| sites(std::slice::from_ref(station)))
    });
    let site = registered
        .as_ref()
        .and_then(|map| map.get(&found.issued.receipt.evse))
        .or_else(|| shared.sites.get(&found.issued.receipt.evse));
    let markup = render(&found, &evidence, key.as_ref(), site, lang);
    private(
        axum::response::Html(markup).into_response(),
        ui::CSP_WITH_WASM,
    )
}

async fn file<J: Journal>(
    Extract(shared): Extract<Arc<Shared<J>>>,
    UrlPath(token): UrlPath<String>,
) -> Response {
    if !shared.window.admits() {
        return too_many();
    }
    let Some(found) = find(&shared, &token) else {
        return not_found();
    };
    let evidence = evidence(&shared.csmsd, &found.issued.receipt);
    if evidence.verified.is_empty() {
        return not_found();
    }
    let Ok(xml) = emob_eichrecht::transparency::to_xml(&evidence) else {
        return not_found();
    };
    let mut response = private(xml.into_response(), ui::CSP);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"emob-transparenzdatei.xml\""),
    );
    response
}

async fn verifier() -> Response {
    let mut response = ui::hardened(VERIFIER.into_response(), ui::CSP);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/wasm"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    response
}

async fn script() -> Response {
    let mut response = ui::hardened(SCRIPT.into_response(), ui::CSP);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/javascript; charset=utf-8"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    response
}

/// The journalled records, replayed against the registry at the session's
/// instant (SR-069).
fn evidence<J: Journal>(csmsd: &Csmsd<J>, receipt: &Receipt) -> Evidence {
    let records: Vec<Record<'_>> = receipt
        .signed
        .iter()
        .filter_map(|text| Record::parse(text).ok())
        .collect();
    Evidence::assemble(
        &records,
        csmsd.registry(),
        receipt.at.unwrap_or(time::OffsetDateTime::UNIX_EPOCH),
    )
}

/// The key the registry held for the session's meter at its start.
fn registered_key<J: Journal>(csmsd: &Csmsd<J>, receipt: &Receipt) -> Option<PublicKey> {
    let at = receipt.at?;
    let first = receipt.signed.first()?;
    let record = Record::parse(first).ok()?;
    csmsd
        .registry()
        .key_for_record(&record, at)
        .ok()
        .map(|registered| registered.key.clone())
}

// ── The page ────────────────────────────────────────────────────────────────

/// German decimals with a comma; English as written.
pub(crate) fn number(text: &str, lang: Lang) -> String {
    match lang {
        Lang::De => text.replace('.', ","),
        Lang::En => text.to_owned(),
    }
}

pub(crate) fn money(amount: Decimal, currency: emob_core::Currency, lang: Lang) -> String {
    let rounded = emob_core::Money::new(amount, currency).round_to_minor_unit();
    let code = currency.to_string();
    let symbol = if code == "EUR" { "€" } else { code.as_str() };
    // A no-break space: an amount never breaks from its currency.
    format!(
        "{}\u{a0}{symbol}",
        number(&rounded.amount().to_string(), lang)
    )
}

pub(crate) fn when(at: time::OffsetDateTime, zone: &TimeZone, lang: Lang) -> String {
    let local = zone.local(at);
    let (d, t) = (local.date, local.time);
    match lang {
        Lang::De => format!(
            "{:02}.{:02}.{}, {:02}:{:02}",
            d.day(),
            u8::from(d.month()),
            d.year(),
            t.hour(),
            t.minute()
        ),
        Lang::En => format!(
            "{}-{:02}-{:02} {:02}:{:02}",
            d.year(),
            u8::from(d.month()),
            d.day(),
            t.hour(),
            t.minute()
        ),
    }
}

/// The end of a range: only the time when it ends on the day it began.
fn until(
    from: time::OffsetDateTime,
    to: time::OffsetDateTime,
    zone: &TimeZone,
    lang: Lang,
) -> String {
    let (start, end) = (zone.local(from), zone.local(to));
    if start.date == end.date {
        format!("{:02}:{:02}", end.time.hour(), end.time.minute())
    } else {
        when(to, zone, lang)
    }
}

fn span(duration: time::Duration) -> String {
    let minutes = duration.whole_minutes().max(0);
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// What a finding means for the driver, in one sentence.
const fn finding_words(kind: FindingKind) -> (&'static str, &'static str) {
    match kind {
        FindingKind::NoKey => (
            "Für den Zähler war kein Schlüssel hinterlegt, also lässt sich der Messwert nicht prüfen.",
            "No key was on file for the meter, so the measured value cannot be checked.",
        ),
        FindingKind::Signature => (
            "Eine Signatur passt nicht zum Messwert.",
            "A signature does not match the measured value.",
        ),
        FindingKind::Unreadable => (
            "Ein signierter Datensatz ist nicht lesbar.",
            "A signed record cannot be read.",
        ),
        FindingKind::MeterState => (
            "Der Zähler hat einen Fehler gemeldet.",
            "The meter reported a fault.",
        ),
        FindingKind::Clock => (
            "Die Uhr des Zählers war nicht synchron, die Dauer ist nicht abrechenbar.",
            "The meter's clock was not synchronised, so the duration cannot be billed.",
        ),
        FindingKind::Register => (
            "Der Zähler hat ein Register gemeldet, das nicht abgerechnet werden darf.",
            "The meter reported a register that may not be billed.",
        ),
        FindingKind::Unit => (
            "Die Messwerte stehen nicht in einer Energieeinheit.",
            "The readings are not in one energy unit.",
        ),
        FindingKind::Directions => (
            "Energie ist in beide Richtungen geflossen.",
            "Energy flowed in both directions.",
        ),
        FindingKind::CableLoss => (
            "Die Kompensation des Leitungsverlusts ist nicht nachvollziehbar.",
            "The cable-loss compensation cannot be traced.",
        ),
        FindingKind::Identification => (
            "Die Identifizierung hat sich während des Ladens geändert oder ist fehlgeschlagen.",
            "The identification changed or failed during charging.",
        ),
        FindingKind::Sequence => (
            "Signierte Messwerte fehlen oder stehen nicht in der richtigen Reihenfolge.",
            "Signed values are missing or out of order.",
        ),
        FindingKind::NoEnergy => (
            "Es gibt kein Paar signierter Zählerstände, aus dem sich die Energie ergibt.",
            "There is no pair of signed readings the energy could be taken from.",
        ),
    }
}

fn dimension(d: Dimension, lang: Lang) -> &'static str {
    match d {
        Dimension::Energy => lang.pick("Energie", "Energy"),
        Dimension::Time => lang.pick("Ladezeit", "Charging time"),
        Dimension::ParkingTime => lang.pick("Standzeit", "Parking time"),
        Dimension::Flat => lang.pick("Grundpreis", "Session fee"),
    }
}

/// One signed value, as the server reads it — the device reads it again.
struct Value<'a> {
    text: &'a str,
    /// `(marker, "RV RU")` for every reading the record states.
    readings: Vec<(char, String)>,
}

fn values(receipt: &Receipt) -> Vec<Value<'_>> {
    receipt
        .signed
        .iter()
        .map(|text| Value {
            text,
            // Parsed only, for the label: the verdict is the device's to give.
            readings: emob_verify::check(text, "")
                .readings
                .into_iter()
                .map(|r| (r.marker, format!("{} {}", r.value, r.unit)))
                .collect(),
        })
        .collect()
}

fn marker_words(marker: char, lang: Lang) -> &'static str {
    match marker {
        'B' => lang.pick("Beginn", "Begin"),
        'E' | 'L' | 'R' | 'A' | 'P' => lang.pick("Ende", "End"),
        'T' => lang.pick("Tarifwechsel", "Tariff change"),
        'S' => lang.pick("Pausiert", "Suspended"),
        _ => lang.pick("Zwischenwert", "Intermediate"),
    }
}

/// The key, hex, in groups of four — the form a label prints.
fn grouped(key: &PublicKey) -> String {
    let hex = hex::encode_upper(key.sec1_bytes());
    hex.as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

#[allow(clippy::too_many_lines)] // One page, top to bottom, in reading order.
fn render(
    found: &Found,
    evidence: &Evidence,
    key: Option<&PublicKey>,
    site: Option<&Site>,
    lang: Lang,
) -> String {
    let receipt = &found.issued.receipt;
    let zone = site.map_or_else(TimeZone::utc, |s| s.zone.clone());
    let mut b = String::with_capacity(8 * 1024);

    // 1. The verdict.
    match &found.verdict {
        Verdict::Billed(_) => {
            let _ = write!(
                b,
                "<section class=\"verdict tone-good\" aria-labelledby=\"v\"><span aria-hidden=\"true\" class=\"icon\">✓</span>\
                 <div><h1 id=\"v\">{}</h1><p>{} <span class=\"cite\">[MessEG §33]</span></p></div></section>",
                lang.pick("Gemessen, signiert, geprüft", "Measured, signed, checked"),
                lang.pick(
                    "Der Zähler hat Beginn und Ende signiert, und die Signaturen passen zum Schlüssel dieses Ladepunkts. Abgerechnet wurde genau dieser Messwert.",
                    "The meter signed the begin and the end, and the signatures match this charge point's key. Exactly this measured value was billed."
                ),
            );
        }
        Verdict::Refused { reasons, findings } => {
            let (why, cite) = findings.first().map_or_else(
                || {
                    (
                        reasons.first().map_or_else(String::new, |r| escape(r)),
                        "[MessEG §33]",
                    )
                },
                |f| {
                    let (de, en) = finding_words(f.kind);
                    (lang.pick(de, en).to_owned(), f.kind.citation())
                },
            );
            // The finding's own paragraph, and `[MessEG §33]` — why a value
            // nobody can check is not billed — once, even where they are one.
            let cites = if cite == "[MessEG §33]" {
                format!("<span class=\"cite\">{}</span>", escape(cite))
            } else {
                format!(
                    "<span class=\"cite\">{}</span> <span class=\"cite\">[MessEG §33]</span>",
                    escape(cite)
                )
            };
            let _ = write!(
                b,
                "<section class=\"verdict tone-bad\" aria-labelledby=\"v\"><span aria-hidden=\"true\" class=\"icon\">✗</span>\
                 <div><h1 id=\"v\">{}</h1><p>{why} {cites}</p>\
                 <p class=\"muted\">{}</p></div></section>",
                lang.pick("Nicht abgerechnet", "Not billed"),
                lang.pick(
                    "Ein Messwert, den Sie nicht prüfen können, darf nicht berechnet werden. Für diesen Ladevorgang wird Ihnen nichts in Rechnung gestellt.",
                    "A measured value you cannot check may not be charged. You are not billed for this session."
                ),
            );
            for finding in findings.iter().skip(1) {
                let (de, en) = finding_words(finding.kind);
                let _ = write!(
                    b,
                    "<p class=\"muted\">{} <span class=\"cite\">{}</span></p>",
                    lang.pick(de, en),
                    escape(finding.kind.citation())
                );
            }
        }
    }

    // 2. The energy, the duration, where.
    let _ = write!(b, "<div class=\"stats receipt-stats\">");
    if let Some(cdr) = &found.cdr {
        let _ = write!(
            b,
            "<div class=\"stat\"><span class=\"muted\">{}</span><span class=\"value\">{} kWh</span></div>\
             <div class=\"stat\"><span class=\"muted\">{}</span><span class=\"value\">{}</span></div>",
            lang.pick("Geladen", "Charged"),
            escape(&number(&cdr.total_energy.kwh().to_string(), lang)),
            lang.pick("Dauer", "Duration"),
            escape(&span(cdr.duration())),
        );
        if let Some(cost) = &cdr.cost {
            let _ = write!(
                b,
                "<div class=\"stat\"><span class=\"muted\">{}</span><span class=\"value\">{}</span></div>",
                lang.pick("Betrag", "Amount"),
                escape(&money(cost.gross().amount(), cost.rated.currency, lang)),
            );
        }
    }
    let _ = write!(b, "</div><dl class=\"kv\">");
    if let Some(cdr) = &found.cdr {
        let _ = write!(
            b,
            "<dt>{}</dt><dd><time>{}</time> – <time>{}</time></dd>",
            lang.pick("Zeitraum", "When"),
            escape(&when(cdr.started_at, &zone, lang)),
            escape(&until(cdr.started_at, cdr.ended_at, &zone, lang)),
        );
    } else if let Some(at) = receipt.at {
        let _ = write!(
            b,
            "<dt>{}</dt><dd><time>{}</time></dd>",
            lang.pick("Beginn", "Started"),
            escape(&when(at, &zone, lang)),
        );
    }
    if let Some(site) = site {
        let _ = write!(
            b,
            "<dt>{}</dt><dd>{}<br>{}</dd>",
            lang.pick("Ort", "Where"),
            escape(&site.name),
            escape(&site.address),
        );
    }
    let _ = write!(
        b,
        "<dt>{}</dt><dd class=\"num\">{}</dd></dl>",
        lang.pick("Ladepunkt", "Ladepunkt (EVSE)"),
        escape(&receipt.evse.to_string()),
    );

    // 3. The price, line by line, from the CDR.
    if let Some(cost) = found.cdr.as_ref().and_then(|cdr| cdr.cost.as_ref()) {
        let rated = &cost.rated;
        let currency = rated.currency;
        let _ = write!(
            b,
            "<h2>{}</h2><div class=\"scroll\"><table class=\"price\"><thead><tr><th>{}</th><th class=\"num\">{}</th></tr></thead><tbody>",
            lang.pick("Preis", "Price"),
            lang.pick("Position", "Item"),
            lang.pick("Betrag", "Amount"),
        );
        for line in &rated.lines {
            let quantity = match line.dimension {
                Dimension::Energy => format!(
                    "{} kWh",
                    number(&line.quantity.normalize().to_string(), lang)
                ),
                Dimension::Time | Dimension::ParkingTime => span(time::Duration::seconds(
                    line.base_quantity.trunc().to_i64().unwrap_or(0),
                )),
                Dimension::Flat => "1".to_owned(),
            };
            let unit = match line.dimension {
                Dimension::Energy => "/kWh",
                Dimension::Time | Dimension::ParkingTime => "/h",
                Dimension::Flat => "",
            };
            let _ = write!(
                b,
                "<tr><td>{}<br><span class=\"muted num\">{} × {} {}{unit}</span></td><td class=\"num\">{}</td></tr>",
                dimension(line.dimension, lang),
                escape(&quantity),
                escape(&number(&line.unit_price.normalize().to_string(), lang)),
                if currency.to_string() == "EUR" {
                    "€".to_owned()
                } else {
                    escape(&currency.to_string())
                },
                escape(&money(line.amount, currency, lang)),
            );
        }
        let _ = write!(b, "</tbody><tfoot>");
        for tax in rated.tax_summary() {
            let label = match rated.tax_included {
                TaxIncluded::Yes => lang.pick("enthaltene USt", "VAT included"),
                TaxIncluded::No | TaxIncluded::NotApplicable => lang.pick("zzgl. USt", "plus VAT"),
            };
            let _ = write!(
                b,
                "<tr><td>{label} {} %</td><td class=\"num\">{}</td></tr>",
                escape(&number(&tax.rate.normalize().to_string(), lang)),
                escape(&money(tax.tax, currency, lang)),
            );
        }
        if let Some(reservation) = &cost.reservation {
            let _ = write!(
                b,
                "<tr><td>{}</td><td class=\"num\">{}</td></tr>",
                lang.pick("Reservierung", "Reservation"),
                escape(&money(reservation.gross().amount(), currency, lang)),
            );
        }
        let _ = write!(
            b,
            "<tr class=\"total\"><th scope=\"row\">{}</th><td class=\"num\">{}</td></tr></tfoot></table></div>",
            lang.pick("Gesamt", "Total"),
            escape(&money(cost.gross().amount(), currency, lang)),
        );
        // The version that rated this session, by the code the price page and
        // the station's display showed before it started (spec 015).
        let _ = write!(
            b,
            "<p class=\"preisstand\"><span class=\"muted\">Preisstand</span> <strong>{code}</strong> · \
             <a href=\"{page}\">{link}</a></p>",
            code = escape(&cost.tariff_fingerprint.code()),
            page = escape(&crate::price::path(&found.issued.receipt.evse)),
            link = lang.pick("Preis dieses Ladepunkts", "This charge point's price"),
        );
    }

    // 4. The signed values, each with a slot the self-check fills.
    let shown = values(receipt);
    let _ = write!(
        b,
        "<h2>{}</h2>",
        lang.pick("Signierte Messwerte", "Signed values")
    );
    if shown.is_empty() {
        let _ = write!(
            b,
            "<p class=\"empty\">{}</p>",
            lang.pick(
                "Für diesen Ladevorgang ist kein signierter Messwert angekommen.",
                "No signed value arrived for this session."
            )
        );
    } else {
        let _ = write!(b, "<ol class=\"values\">");
        for value in &shown {
            let readings = value
                .readings
                .iter()
                .map(|(marker, reading)| {
                    format!(
                        "<span class=\"reading\"><span class=\"what\">{}</span> <span class=\"num\">{}</span></span>",
                        marker_words(*marker, lang),
                        escape(&number(reading, lang))
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            let _ = write!(
                b,
                "<li data-ocmf=\"{}\">{readings}\
                 <span class=\"said muted\"></span><span class=\"check badge\" aria-live=\"polite\"></span></li>",
                escape(value.text),
            );
        }
        let _ = write!(b, "</ol>");
    }

    // 5. The meter's key: the one trust anchor a driver has.
    let _ = write!(b, "<h2>{}</h2>", lang.pick("Zähler-Schlüssel", "Meter key"));
    if let Some(key) = key {
        let _ = write!(
            b,
            "<p>{}</p><p class=\"key num\" translate=\"no\">{}</p><p class=\"muted\">{} {}</p>",
            lang.pick(
                "Vergleichen Sie den vollständigen Schlüssel mit dem Schlüssel, der am Ladepunkt aufgedruckt ist. Das Prüfergebnis für die Signaturen steht bei den Messwerten.",
                "Compare the complete key with the one printed on the charge point. Each signed reading shows its own verification result."
            ),
            escape(&grouped(key)),
            lang.pick("Kurve", "Curve"),
            escape(key.curve().name()),
        );
    } else {
        let _ = write!(
            b,
            "<p class=\"empty\">{}</p>",
            lang.pick(
                "Für diesen Zähler ist kein Schlüssel hinterlegt.",
                "No key is on file for this meter."
            )
        );
    }

    // 6. The file, for the official verifier.
    if !evidence.verified.is_empty() {
        let _ = write!(
            b,
            "<p><a class=\"btn ghost\" href=\"/r/{}/transparenz.xml\" download>{}</a></p><p class=\"muted\">{}</p>",
            escape(&found.token),
            lang.pick(
                "Transparenzdatei herunterladen",
                "Download the transparency file"
            ),
            lang.pick(
                "Für die S.A.F.E.-Transparenzsoftware, die amtlich anerkannte Prüfsoftware.",
                "For the S.A.F.E. Transparenzsoftware, the officially recognised verifier."
            ),
        );
    }

    // 7. Check it yourself — on this device.
    if let (Some(key), false) = (key, shown.is_empty()) {
        let _ = write!(
            b,
            "<section id=\"self-check\" class=\"card\" data-wasm=\"{wasm}\" data-key=\"{key}\" \
             data-w0=\"{w0}\" data-w1=\"{w1}\" data-w2=\"{w2}\" data-w3=\"{w3}\" data-w4=\"{w4}\" data-w5=\"{w5}\" \
             data-all=\"{all}\" data-some=\"{some}\" data-fail=\"{fail}\">\
             <h2>{title}</h2><p>{explain}</p>\
             <p class=\"nojs muted\">{nojs}</p>\
             <p><button type=\"button\" class=\"btn\" id=\"self-check-run\" hidden>{button}</button></p>\
             <p class=\"summary\" role=\"status\"></p></section>",
            wasm = VERIFIER_PATH,
            key = hex::encode_upper(key.sec1_bytes()),
            w0 = lang.pick("signiert und unverändert", "signed and unchanged"),
            w1 = lang.pick("Signatur passt nicht — der Wert wurde verändert", "signature does not match — the value was changed"),
            w2 = lang.pick("im Browser nicht prüfbar — bitte die Transparenzsoftware verwenden", "not verifiable in the browser — use the Transparenzsoftware"),
            w3 = lang.pick("Datensatz nicht lesbar", "record unreadable"),
            w4 = lang.pick("Schlüssel nicht lesbar", "key unreadable"),
            w5 = lang.pick("Schlüssel passt nicht zum Verfahren", "key does not fit the algorithm"),
            all = lang.pick("Alle Messwerte sind auf Ihrem Gerät geprüft: signiert und unverändert.", "Every value was checked on your device: signed and unchanged."),
            some = lang.pick("Nicht jeder Messwert ließ sich bestätigen — siehe oben.", "Not every value could be confirmed — see above."),
            fail = lang.pick("Der Prüfer ließ sich nicht laden.", "The verifier could not be loaded."),
            title = lang.pick("Selbst prüfen", "Check it yourself"),
            explain = lang.pick(
                "Ihr Gerät prüft jede Signatur selbst, mit einer WebAssembly-Fassung der OCMF-Prüfung — nichts auf diesem Server muss stimmen. Ist die Seite geladen, funktioniert das auch offline. Dasselbe Modul prüft jeden OCMF-Wert, auch ohne Beleg, unter hupe1980.github.io/emob/verify.",
                "Your device checks every signature itself, with a WebAssembly build of the OCMF check — nothing on this server has to be trusted. Once the page has loaded, this works offline too. The same module checks any OCMF value, without a receipt, at hupe1980.github.io/emob/verify."
            ),
            nojs = lang.pick("Dafür braucht es JavaScript; die Transparenzdatei geht auch ohne.", "This needs JavaScript; the transparency file works without it."),
            button = lang.pick("Selbst prüfen", "Check it myself"),
        );
    }

    // 8. What this page does not show.
    let other = match lang {
        Lang::De => "<a href=\"?lang=en\" hreflang=\"en\" lang=\"en\">English</a>",
        Lang::En => "<a href=\"?lang=de\" hreflang=\"de\" lang=\"de\">Deutsch</a>",
    };
    let _ = write!(
        b,
        "<footer class=\"foot\"><p>{}</p><p>{other}</p></footer>",
        lang.pick(
            "Dieser Beleg zeigt keinen Namen, keine Kartennummer und keinen Vertrag. Wer den Link hat, kann ihn öffnen — geben Sie ihn nur weiter, wenn Sie das wollen. Er ersetzt keine Rechnung.",
            "This receipt shows no name, no card number and no contract. Anyone with the link can open it — share it only if you want to. It is not an invoice."
        ),
    );

    ui::page(
        &Shell {
            title: lang.pick("Ladebeleg", "Charging receipt"),
            place: lang.pick("Ladebeleg", "Charging receipt"),
            lang,
            reading: true,
            script: Some(SCRIPT_PATH),
        },
        &b,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_service::journal::Memory;
    use tower_service::Service as _;

    struct Nobody;

    impl crate::Authorize for Nobody {
        fn ask<'a>(
            &'a self,
            _uid: &'a str,
            _evse: Option<&'a emob_core::EvseId>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::Answer> + Send + 'a>>
        {
            Box::pin(async { unreachable!("no station connects in these tests") })
        }
    }

    fn csmsd() -> Arc<Csmsd<Memory>> {
        Arc::new(
            Csmsd::open(
                emob_core::PartyId::new("DE", "ABC").unwrap(),
                emob_eichrecht::KeyRegistry::new(),
                Arc::new(Nobody),
                Memory::new(),
                emob_core::TimeZone::utc(),
            )
            .unwrap()
            .issuing_receipts(Receipts::ephemeral("https://csms.example/").unwrap()),
        )
    }

    fn refused(reason: &str) -> Found {
        Found {
            token: "A".repeat(TOKEN_LEN),
            issued: Issued {
                key: ("CP-1".into(), "7".into()),
                receipt: Receipt {
                    digest: digest(&"A".repeat(TOKEN_LEN)),
                    evse: "DE*ABC*E00001".parse().unwrap(),
                    at: None,
                    signed: vec!["OCMF|{\"x\":\"<b>\"}|{}".to_owned()],
                },
                verdict: None,
            },
            verdict: Verdict::Refused {
                reasons: vec![reason.to_owned()],
                findings: Vec::new(),
            },
            cdr: None,
        }
    }

    #[test]
    fn sr_065_every_stored_value_is_escaped() {
        let found = refused("<script>alert(1)</script>");
        let html = render(
            &found,
            &Evidence::assemble(
                &[],
                &emob_eichrecht::KeyRegistry::new(),
                time::OffsetDateTime::UNIX_EPOCH,
            ),
            None,
            None,
            Lang::De,
        );
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert"));
        assert!(
            !html.contains("<b>"),
            "a record's text is an attribute value, escaped"
        );
        // The one script is the same-origin module.
        assert_eq!(html.matches("<script").count(), 1);
        assert!(html.contains("<script type=\"module\" src=\"/r/verify.js\">"));
        assert!(html.contains("Nicht abgerechnet"));
        assert!(html.contains("[MessEG §33]"));
    }

    #[test]
    fn a_refused_receipt_cites_each_paragraph_once() {
        let render_with = |kind| {
            let mut found = refused("the signature does not verify");
            found.verdict = Verdict::Refused {
                reasons: vec!["the signature does not verify".to_owned()],
                findings: vec![emob_eichrecht::Finding {
                    kind,
                    takes: emob_eichrecht::Disqualifies::Both,
                    detail: String::new(),
                }],
            };
            render(
                &found,
                &Evidence::assemble(
                    &[],
                    &emob_eichrecht::KeyRegistry::new(),
                    time::OffsetDateTime::UNIX_EPOCH,
                ),
                None,
                None,
                Lang::De,
            )
        };
        // A signature that fails rests on `[MessEG §33]` itself: once.
        let html = render_with(emob_eichrecht::FindingKind::Signature);
        assert!(!html.contains("[MessEG §33]</span> <span class=\"cite\">[MessEG §33]"));
        assert_eq!(html.matches("[MessEG §33]").count(), 1, "{html}");
        // A clock fault cites its own table, and §33 beside it.
        let html = render_with(emob_eichrecht::FindingKind::Clock);
        assert!(html.contains("[OCMF Tab. 19]"));
        assert_eq!(html.matches("[MessEG §33]").count(), 1, "{html}");
    }

    #[test]
    fn sr_060_the_token_is_128_bits_deterministic_and_keyed() {
        let receipts = Receipts::ephemeral("https://csms.example/").unwrap();
        let key = ("CP-1".to_owned(), "7".to_owned());
        let token = receipts.token(&key);
        assert_eq!(token.len(), TOKEN_LEN);
        assert!(well_formed(&token));
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&token)
                .unwrap()
                .len(),
            16
        );
        assert_eq!(
            receipts.token(&key),
            token,
            "printable again on a later invoice"
        );
        assert_ne!(receipts.token(&("CP-1".into(), "8".into())), token);
        // `CP-1` + `7` and `CP-17` + `` are different transactions.
        assert_ne!(
            receipts.token(&("CP-17".into(), String::new())),
            receipts.token(&("CP-1".into(), "7".into()))
        );
        let other = Receipts::ephemeral("https://csms.example/").unwrap();
        assert_ne!(other.token(&key), token, "another key, another token");
        assert_eq!(
            receipts.url(&key),
            format!("https://csms.example/r/{token}")
        );
        assert_eq!(digest(&token).len(), 64);
        assert!(!format!("{receipts:?}").contains(&hex::encode(receipts.key)));
    }

    #[test]
    fn sr_060_sr_062_the_key_file_is_created_once_and_private() {
        let dir = std::env::temp_dir().join(format!("emob-receipt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let first = Receipts::open(&dir, "https://csms.example").unwrap();
        let again = Receipts::open(&dir, "https://csms.example").unwrap();
        let key = ("CP-1".to_owned(), "1".to_owned());
        assert_eq!(
            first.token(&key),
            again.token(&key),
            "a restart keeps the links"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.join(KEY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::write(dir.join(KEY_FILE), b"short").unwrap();
        assert!(
            Receipts::open(&dir, "https://csms.example").is_err(),
            "refused, not replaced"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sr_067_beyond_the_window_the_answer_is_429_without_rendering() {
        let mut router = router_limited(csmsd(), BTreeMap::new(), 2);
        let mut statuses = Vec::new();
        for _ in 0..3 {
            let response = router
                .call(
                    axum::http::Request::builder()
                        .uri("/r/AAAAAAAAAAAAAAAAAAAAAA")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            statuses.push((
                response.status(),
                response.headers().get(header::RETRY_AFTER).cloned(),
            ));
        }
        assert_eq!(statuses[0].0, StatusCode::NOT_FOUND);
        assert_eq!(statuses[1].0, StatusCode::NOT_FOUND);
        assert_eq!(statuses[2].0, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses[2].1.as_ref().unwrap(), "1");
    }

    #[test]
    fn the_verifier_compiled_in_is_a_module_inside_its_budget() {
        assert_eq!(&VERIFIER[..4], b"\0asm");
        // 200 kB compressed is the budget; `just wasm` measures the gzip. Raw
        // is checked here so a build that doubled it fails a test too.
        assert!(VERIFIER.len() < 450 * 1024, "{} bytes", VERIFIER.len());
        assert!(!SCRIPT.contains("http://") && !SCRIPT.contains("https://"));
        assert!(SCRIPT.lines().count() <= 45, "forty lines, not a framework");
    }
}
