//! `/i/{token}` — the invoice its recipient reads (spec 010).
//!
//! From 01.01.2027 a business invoice is the structured file `[UStG §14(1)]`
//! names — EN 16931 in UBL or CII — and nobody reads XML. So each issued
//! document gets one page, built like a well-designed bill: what is owed, by
//! when and how it is collected first; then who bills whom; then every session
//! with the receipt that lets the driver check it; the VAT; the one rounding
//! amount the document states (BT-114) said in a sentence; and the two files,
//! which are the invoice in law. German first, `?lang=en`, printable, no script.
//!
//! # The page computes nothing that decides
//!
//! Every figure is the document's own — `payable_total`, `tax`, `net`,
//! `payable_rounding` — and the files are `emob_billing::en16931::write` in the
//! specification the document was issued under, byte for byte what the
//! operator system receives ([`crate::deliver`]).
//!
//! # The token (SR-116)
//!
//! The receipt's rules (spec 003), with billd's own key:
//! `base64url(HMAC-SHA256(invoice.key, "emob-invoice\0" ‖ number)[..16])` —
//! 128 bits, 22 characters, the key 32 bytes from the OS CSPRNG in
//! `invoice.key` beside the journal and never in it. Deterministic, so a link
//! can be sent again. Nothing about it is journalled: a request recomputes the
//! token of each document — partners × months, not sessions — and compares
//! digests. An unknown or malformed token is the router's own empty 404.
//!
//! # What it shows, and what it does not (SR-117)
//!
//! Only what the invoice carries, behind its token: the parties, the sessions,
//! the amounts. A debtor's IBAN is masked to its country and last four. And a
//! receipt does **not** link here: the invoice is a partner's month, every
//! driver of that partner on it, and a back-link would widen one driver's
//! one-session link to all of them.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine as _;
use emob_billing::invoice::{Counterparty, Invoice, PaymentDetails};
use emob_billing::{Specification, Syntax};
use emob_service::journal::Journal;
use emob_service::ui::{self, Lang, Shell, escape};
use emob_tariff::Dimension;
use hmac::{Hmac, KeyInit as _, Mac as _};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive as _;
use sha2::{Digest as _, Sha256};

use crate::{Books, Issued};

/// The invoice key's file, beside the journal.
pub const KEY_FILE: &str = "invoice.key";

/// How many requests per second `/i/` takes before it answers 429 (SR-118).
pub const PER_SECOND: u32 = 60;

/// A token is 16 bytes, base64url without padding.
const TOKEN_LEN: usize = 22;

/// The key invoice links are minted under, and where they point.
pub struct Invoices {
    key: [u8; 32],
    public_url: Option<String>,
}

impl core::fmt::Debug for Invoices {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Invoices")
            .field("public_url", &self.public_url)
            .finish_non_exhaustive()
    }
}

impl Invoices {
    /// The key in `dir/invoice.key`, created from the OS CSPRNG with mode 0600
    /// the first time (SR-116).
    ///
    /// # Errors
    ///
    /// The file cannot be read or created, holds something other than 32
    /// bytes, or the OS has no randomness to give.
    pub fn open(dir: &Path, public_url: Option<&str>) -> std::io::Result<Self> {
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

    /// A key drawn now and kept nowhere — for links that need not outlive the
    /// process (tests).
    ///
    /// # Errors
    ///
    /// The OS has no randomness to give.
    pub fn ephemeral(public_url: Option<&str>) -> std::io::Result<Self> {
        Ok(Self::with_key(fresh()?, public_url))
    }

    fn with_key(key: [u8; 32], public_url: Option<&str>) -> Self {
        Self {
            key,
            public_url: public_url.map(|url| url.trim_end_matches('/').to_owned()),
        }
    }

    /// The token for one document.
    #[must_use]
    pub fn token(&self, number: &str) -> String {
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&self.key) else {
            unreachable!("HMAC takes a key of any length")
        };
        mac.update(b"emob-invoice\0");
        mac.update(number.as_bytes());
        let tag = mac.finalize().into_bytes();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&tag[..16])
    }

    /// `/i/{token}`.
    #[must_use]
    pub fn path(&self, number: &str) -> String {
        format!("/i/{}", self.token(number))
    }

    /// The absolute link, where the estate states the origin.
    #[must_use]
    pub fn url(&self, number: &str) -> Option<String> {
        self.public_url
            .as_ref()
            .map(|origin| format!("{origin}{}", self.path(number)))
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

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn well_formed(token: &str) -> bool {
    token.len() == TOKEN_LEN
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The document in one syntax, in the specification it was issued under —
/// the bytes the page serves and the operator system receives (SR-119).
///
/// # Errors
///
/// What [`emob_billing::en16931::write`] refuses — which a closing already
/// checked (SR-122), so for a document billd issued this does not happen.
pub fn file(issued: &Issued, syntax: Syntax) -> Result<String, emob_billing::BillingError> {
    emob_billing::en16931::write(&issued.invoice, issued.specification, syntax)
        .map(|crossing| crossing.value)
}

// ── The window ──────────────────────────────────────────────────────────────

/// One global fixed window (SR-118): the 128-bit token stops enumeration; this
/// bounds what a flood costs a daemon that also receives deliveries.
#[derive(Debug)]
struct Window {
    limit: u32,
    held: Mutex<(Instant, u32)>,
}

impl Window {
    fn new(limit: u32) -> Self {
        Self {
            limit,
            held: Mutex::new((Instant::now(), 0)),
        }
    }

    fn admits(&self) -> bool {
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
    books: Arc<Mutex<Books<J>>>,
    invoices: Arc<Invoices>,
    window: Window,
}

/// The invoice routes and the shared stylesheet, unauthenticated.
pub fn router<J: Journal>(books: Arc<Mutex<Books<J>>>, invoices: Arc<Invoices>) -> Router {
    router_limited(books, invoices, PER_SECOND)
}

/// [`router`] with another window — for a test that floods it.
pub fn router_limited<J: Journal>(
    books: Arc<Mutex<Books<J>>>,
    invoices: Arc<Invoices>,
    limit: u32,
) -> Router {
    let shared = Arc::new(Shared {
        books,
        invoices,
        window: Window::new(limit),
    });
    Router::new()
        .route("/i/{token}", get(page::<J>))
        .route("/i/{token}/ubl.xml", get(ubl::<J>))
        .route("/i/{token}/cii.xml", get(cii::<J>))
        .with_state(shared)
        .merge(ui::assets())
}

#[derive(serde::Deserialize)]
struct LangQuery {
    lang: Option<String>,
}

/// The router's own answer for a path it does not serve (SR-116): an empty
/// body and no header, so not even a `content-length` tells them apart.
fn not_found() -> Response {
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

/// The headers every invoice answer carries beyond [`ui::hardened`] (SR-117).
fn private(response: Response) -> Response {
    let mut response = ui::hardened(response, ui::CSP);
    response.headers_mut().insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    response
}

/// What one token opens: the document, and what cancels it.
struct Found {
    token: String,
    issued: Issued,
    cancelled_by: Option<String>,
}

fn find<J: Journal>(shared: &Shared<J>, token: &str) -> Option<Found> {
    if !well_formed(token) {
        return None;
    }
    let wanted = digest(token);
    let books = shared.books.lock().unwrap_or_else(PoisonError::into_inner);
    let state = books.state();
    let issued = state
        .documents()
        .find(|issued| digest(&shared.invoices.token(&issued.invoice.number)) == wanted)?;
    Some(Found {
        token: token.to_owned(),
        cancelled_by: state
            .cancelled_by(&issued.invoice.number)
            .map(str::to_owned),
        issued: issued.clone(),
    })
}

async fn page<J: Journal>(
    State(shared): State<Arc<Shared<J>>>,
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
    private(axum::response::Html(render(&found, lang)).into_response())
}

async fn ubl<J: Journal>(
    State(shared): State<Arc<Shared<J>>>,
    UrlPath(token): UrlPath<String>,
) -> Response {
    download(&shared, &token, Syntax::Ubl)
}

async fn cii<J: Journal>(
    State(shared): State<Arc<Shared<J>>>,
    UrlPath(token): UrlPath<String>,
) -> Response {
    download(&shared, &token, Syntax::Cii)
}

fn download<J: Journal>(shared: &Shared<J>, token: &str, syntax: Syntax) -> Response {
    if !shared.window.admits() {
        return too_many();
    }
    let Some(found) = find(shared, token) else {
        return not_found();
    };
    let xml = match file(&found.issued, syntax) {
        Ok(xml) => xml,
        Err(error) => {
            tracing::error!(%error, "an issued document could not be written");
            return not_found();
        }
    };
    let mut response = private(xml.into_response());
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml; charset=utf-8"),
    );
    if let Ok(value) = HeaderValue::from_str(&format!(
        "attachment; filename=\"{}\"",
        file_name(&found.issued, syntax)
    )) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

/// `R-2026-0001-xrechnung-ubl.xml` — only `[A-Za-z0-9._-]` from the number.
fn file_name(issued: &Issued, syntax: Syntax) -> String {
    let number: String = issued
        .invoice
        .number
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let spec = match issued.specification {
        Specification::XRechnung => "xrechnung",
        Specification::PeppolBis3 => "peppol",
        _ => "en16931",
    };
    let syntax = match syntax {
        Syntax::Ubl => "ubl",
        Syntax::Cii => "cii",
    };
    format!("{number}-{spec}-{syntax}.xml")
}

// ── The page ────────────────────────────────────────────────────────────────

fn money(amount: Decimal, currency: emob_core::Currency, lang: Lang) -> String {
    let rounded = emob_core::Money::new(amount, currency).round_to_minor_unit();
    let code = currency.to_string();
    let symbol = if code == "EUR" { "€" } else { code.as_str() };
    format!("{} {symbol}", lang.decimal(rounded.amount()))
}

fn span(seconds: Decimal) -> String {
    let minutes = seconds.to_i64().unwrap_or(0).max(0) / 60;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
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

/// How it is paid, in one line — the debtor's IBAN masked (SR-117), the
/// payee's account in full, because a transfer cannot be made to a mask.
fn payment(payment: Option<&PaymentDetails>, lang: Lang) -> Option<String> {
    Some(match payment? {
        PaymentDetails::DirectDebit {
            mandate_reference,
            debited_iban,
            ..
        } => format!(
            "{} <span class=\"num\" translate=\"no\">{}</span> · {} {}",
            lang.pick("SEPA-Lastschrift von", "SEPA direct debit from"),
            escape(&emob_billing::payment::mask_iban(debited_iban)),
            lang.pick("Mandat", "mandate"),
            escape(mandate_reference),
        ),
        PaymentDetails::CreditTransfer { iban, holder } => format!(
            "{} <span class=\"num\" translate=\"no\">{}</span>{}",
            lang.pick("Überweisung an", "Transfer to"),
            escape(&grouped(iban)),
            holder
                .as_ref()
                .map_or_else(String::new, |h| format!(" ({})", escape(h))),
        ),
    })
}

/// An IBAN in groups of four, as it is printed and typed.
fn grouped(iban: &str) -> String {
    let compact: String = iban.chars().filter(|c| !c.is_whitespace()).collect();
    compact
        .as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn party(party: &Counterparty) -> String {
    let mut b = format!("<strong>{}</strong>", escape(&party.name));
    if let Some(street) = &party.street {
        let _ = write!(b, "<br>{}", escape(street));
    }
    let _ = write!(
        b,
        "<br>{}{}",
        party
            .post_code
            .as_ref()
            .map_or_else(String::new, |p| format!("{} ", escape(p))),
        escape(&party.city)
    );
    if let Some(id) = &party.tax.vat_identifier {
        let _ = write!(
            b,
            "<br><span class=\"muted num\">USt-IdNr. {}</span>",
            escape(id)
        );
    }
    b
}

fn spec_label(specification: Specification, lang: Lang) -> &'static str {
    match specification {
        Specification::XRechnung => "XRechnung 3.0",
        Specification::PeppolBis3 => "Peppol BIS 3.0",
        _ => lang.pick("E-Rechnung EN 16931", "E-invoice EN 16931"),
    }
}

#[allow(clippy::too_many_lines)] // One page, top to bottom, in reading order.
fn render(found: &Found, lang: Lang) -> String {
    let invoice: &Invoice = &found.issued.invoice;
    let currency = invoice.currency;
    let credit = invoice.kind.is_credit_note();
    let title = if credit {
        lang.pick("Stornorechnung", "Credit note")
    } else {
        lang.pick("Rechnung", "Invoice")
    };
    let mut b = String::with_capacity(16 * 1024);

    // 1. The verdict: what, how much, by when, how.
    let amount = money(invoice.payable_total().amount(), currency, lang);
    let _ = write!(
        b,
        "<section class=\"verdict tone-info bill\" aria-labelledby=\"v\"><div>\
         <h1 class=\"eyebrow\">{title} <span class=\"num\">{}</span></h1>\
         <p class=\"value num\" id=\"v\">{amount}</p>",
        escape(&invoice.number),
    );
    let mut said = Vec::new();
    if credit {
        if let Some(cancels) = &invoice.cancels {
            said.push(format!(
                "{} <span class=\"num\">{}</span> {}",
                lang.pick("storniert Rechnung", "cancels invoice"),
                escape(&cancels.number),
                lang.pick(
                    "— dieser Betrag wird gutgeschrieben",
                    "— this amount is credited"
                ),
            ));
        }
    } else if let Some(due) = invoice.due_on {
        said.push(format!(
            "{} <time>{}</time>",
            lang.pick("fällig am", "due on"),
            lang.date(due)
        ));
    }
    if let Some(how) = payment(invoice.payment.as_ref(), lang) {
        said.push(how);
    }
    if !said.is_empty() {
        let _ = write!(b, "<p>{}</p>", said.join(" · "));
    }
    let _ = write!(b, "</div></section>");
    if let Some(by) = &found.cancelled_by {
        let _ = write!(
            b,
            "<p class=\"verdict tone-warn\"><span aria-hidden=\"true\" class=\"icon\">!</span>\
             <span><strong>{}</strong> {} <span class=\"num\">{}</span>. {}</span></p>",
            lang.pick("Storniert", "Cancelled"),
            lang.pick("durch", "by"),
            escape(by),
            lang.pick(
                "Dieser Betrag ist nicht mehr fällig.",
                "This amount is no longer due."
            ),
        );
    }

    // 2. Who bills whom, and for when.
    let _ = write!(
        b,
        "<dl class=\"kv\"><dt>{}</dt><dd><time>{}</time></dd><dt>{}</dt><dd><time>{}</time> – <time>{}</time></dd>\
         <dt>{}</dt><dd>{}</dd><dt>{}</dt><dd>{}</dd>",
        lang.pick("Rechnungsdatum", "Issued"),
        lang.date(invoice.issued_on),
        lang.pick("Leistungszeitraum", "Period"),
        lang.date(invoice.period_from),
        lang.date(invoice.period_to),
        lang.pick("Von", "From"),
        party(&invoice.seller),
        lang.pick("An", "To"),
        party(&invoice.buyer),
    );
    if let Some(reference) = &invoice.buyer_reference {
        let _ = write!(
            b,
            "<dt>{}</dt><dd class=\"num\">{}</dd>",
            lang.pick("Ihre Referenz", "Your reference"),
            escape(reference)
        );
    }
    let _ = write!(b, "</dl>");

    // 3. Every session, with the receipt the driver checks it on.
    let sessions = {
        let mut keys: Vec<_> = invoice.lines.iter().map(|l| &l.cdr).collect();
        keys.dedup();
        keys.len()
    };
    let _ = write!(
        b,
        "<h2>{} <span class=\"count\">{sessions}</span></h2><div class=\"scroll\"><table class=\"lines\">\
         <thead><tr><th scope=\"col\">{}</th><th scope=\"col\">{}</th><th scope=\"col\" class=\"num\">{}</th>\
         <th scope=\"col\" class=\"num\">{}</th></tr></thead><tbody>",
        lang.pick("Ladevorgänge", "Sessions"),
        lang.pick("Ladevorgang", "Session"),
        lang.pick("Position", "Item"),
        lang.pick("Menge × Preis", "Quantity × price"),
        lang.pick("Netto", "Net"),
    );
    let mut previous = None;
    for line in &invoice.lines {
        let first = previous != Some(&line.cdr);
        previous = Some(&line.cdr);
        let session = if first {
            let receipt = line.receipt.as_ref().map_or_else(String::new, |url| {
                format!(
                    "<br><a href=\"{}\" rel=\"noreferrer\" class=\"print-url\">{}</a>",
                    escape(url),
                    lang.pick("Beleg prüfen", "Check the receipt"),
                )
            });
            format!(
                "<time>{}</time><br><span class=\"muted num\" translate=\"no\">{}</span>{receipt}",
                lang.date(line.started_at.date()),
                escape(&line.cdr.to_string()),
            )
        } else {
            String::new()
        };
        let quantity = match line.dimension {
            Dimension::Energy => format!("{} kWh", lang.decimal(line.quantity.normalize())),
            Dimension::Time | Dimension::ParkingTime => span(line.quantity),
            Dimension::Flat => "1".to_owned(),
        };
        let unit = match line.dimension {
            Dimension::Energy => "/kWh",
            Dimension::Time | Dimension::ParkingTime => "/h",
            Dimension::Flat => "",
        };
        let _ = write!(
            b,
            "<tr{}><td>{session}</td><td>{}</td><td class=\"num\">{} × {}{unit}</td><td class=\"num\">{}</td></tr>",
            if first { " class=\"first\"" } else { "" },
            dimension(line.dimension, lang),
            escape(&quantity),
            escape(&money_precise(line.unit_price, currency, lang)),
            escape(&money(line.net, currency, lang)),
        );
    }
    let _ = write!(b, "</tbody></table></div>");

    // 4. The VAT, and the totals the standard chains.
    let _ = write!(
        b,
        "<h2>{}</h2><div class=\"scroll\"><table class=\"totals\"><tbody>",
        lang.pick("Summe", "Totals"),
    );
    let row = |b: &mut String, label: &str, value: String| {
        let _ = write!(
            b,
            "<tr><th scope=\"row\">{label}</th><td class=\"num\">{value}</td></tr>"
        );
    };
    row(
        &mut b,
        lang.pick("Positionen netto", "Lines, net"),
        money(invoice.line_total().amount(), currency, lang),
    );
    for adjustment in &invoice.adjustments {
        let label = match adjustment.kind {
            emob_billing::DocumentAdjustmentKind::Allowance => lang.pick("Nachlass", "Allowance"),
            emob_billing::DocumentAdjustmentKind::Charge => lang.pick("Zuschlag", "Charge"),
        };
        row(
            &mut b,
            &format!("{label}: {}", escape(&adjustment.reason)),
            money(adjustment.amount, currency, lang),
        );
    }
    for tax in &invoice.tax {
        let label = tax.rate.map_or_else(
            || format!("{} ({})", lang.pick("USt", "VAT"), tax.category.code()),
            |rate| {
                format!(
                    "{} {} % {} {}",
                    lang.pick("USt", "VAT"),
                    lang.decimal(rate.normalize()),
                    lang.pick("auf", "on"),
                    money(tax.taxable, currency, lang)
                )
            },
        );
        row(&mut b, &escape(&label), money(tax.tax, currency, lang));
    }
    if !invoice.payable_rounding.is_zero() {
        row(
            &mut b,
            lang.pick("Rundung", "Rounding"),
            money(invoice.payable_rounding, currency, lang),
        );
    }
    let _ = write!(
        b,
        "</tbody><tfoot><tr class=\"total\"><th scope=\"row\">{}</th><td class=\"num\">{amount}</td></tr></tfoot></table></div>",
        if credit {
            lang.pick("Gutschrift", "Credited")
        } else {
            lang.pick("Zahlbetrag", "Amount due")
        },
    );
    if !invoice.payable_rounding.is_zero() {
        let _ = write!(
            b,
            "<p class=\"muted\">{} <span class=\"cite\">BT-114</span></p>",
            lang.pick(
                "Jeder Beleg nennt seinen Bruttobetrag auf den Cent gerundet, die Umsatzsteuer wird hier je Steuersatz aus der Nettosumme berechnet — die Rundung gleicht den Zahlbetrag der Summe Ihrer Belege an.",
                "Each receipt states its gross rounded to the cent, while the VAT here is computed per rate from the net total — the rounding brings the amount due to the sum of your receipts."
            ),
        );
    }

    // 5. The files — the invoice in law.
    let spec = spec_label(found.issued.specification, lang);
    let _ = write!(
        b,
        "<h2>{}</h2><p>{}</p><p class=\"noprint downloads\">\
         <a class=\"btn\" href=\"/i/{token}/ubl.xml\" download>{spec} · UBL</a> \
         <a class=\"btn ghost\" href=\"/i/{token}/cii.xml\" download>{spec} · CII</a></p>",
        lang.pick("E-Rechnung", "E-invoice"),
        lang.pick(
            "Maßgeblich ist die strukturierte Rechnung; diese Seite stellt sie dar. Beide Dateien sind dieselbe Rechnung — Ihre Buchhaltung nimmt die eine oder die andere.",
            "The structured invoice is the one that counts; this page shows it. Both files are the same invoice — your accounts system takes one or the other."
        ),
        token = escape(&found.token),
    );

    // 6. What this page is.
    let other = match lang {
        Lang::De => "<a href=\"?lang=en\" hreflang=\"en\" lang=\"en\">English</a>",
        Lang::En => "<a href=\"?lang=de\" hreflang=\"de\" lang=\"de\">Deutsch</a>",
    };
    let _ = write!(
        b,
        "<footer class=\"foot\"><p>{}</p><p class=\"noprint\">{other}</p></footer>",
        lang.pick(
            "Diese Seite zeigt nur, was die Rechnung enthält. Wer den Link hat, kann sie öffnen — geben Sie ihn nur weiter, wenn Sie das wollen.",
            "This page shows only what the invoice contains. Anyone with the link can open it — share it only if you want to."
        ),
    );

    ui::page(
        &Shell {
            title: &format!("{title} {}", invoice.number),
            place: title,
            lang,
            reading: true,
            script: None,
        },
        &b,
    )
}

/// A unit price: BT-146 at full precision is the document's, a bill shows four
/// places.
fn money_precise(amount: Decimal, currency: emob_core::Currency, lang: Lang) -> String {
    let code = currency.to_string();
    let symbol = if code == "EUR" { "€" } else { code.as_str() };
    format!("{} {symbol}", lang.decimal(amount.round_dp(4).normalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_22_url_safe_characters_and_depends_on_the_key() {
        let one = Invoices::ephemeral(None).expect("randomness");
        let two = Invoices::ephemeral(None).expect("randomness");
        let token = one.token("R-2026-0001");
        assert!(well_formed(&token), "{token}");
        assert_eq!(token, one.token("R-2026-0001"), "deterministic");
        assert_ne!(token, two.token("R-2026-0001"), "keyed");
        assert_ne!(token, one.token("R-2026-0002"));
    }

    #[test]
    fn sr_116_the_key_file_is_private_and_reread() {
        let dir = std::env::temp_dir().join(format!("billd-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let _ = std::fs::remove_file(dir.join(KEY_FILE));
        let first = Invoices::open(&dir, Some("https://bill.example/")).expect("created");
        let again = Invoices::open(&dir, None).expect("read back");
        assert_eq!(first.token("R-1"), again.token("R-1"));
        assert_eq!(
            first.url("R-1").as_deref(),
            Some(format!("https://bill.example{}", first.path("R-1")).as_str())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.join(KEY_FILE))
                .expect("the file")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_number_reaches_a_file_name_only_as_safe_characters() {
        assert_eq!(
            grouped("DE89370400440532013000"),
            "DE89 3704 0044 0532 0130 00"
        );
        assert!(well_formed("AAAAAAAAAAAAAAAAAAAAAA"));
        assert!(!well_formed("AAAAAAAAAAAAAAAAAAAAA/"));
    }
}
