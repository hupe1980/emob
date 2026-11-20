//! Handing each issued invoice to the operator system (spec 010).
//!
//! `[UStG §14(1)]` makes the structured file the invoice; billd writes it and
//! does not transmit it — no SMTP, no Peppol access point. What it does is the
//! csmsd outbox's pattern pointed outward: every document whose submission is
//! still `Issued` is posted, signed, to the operator's ERP or Peppol provider,
//! until that system answers. The pending set is the fold's — nothing is
//! journalled to send — and the answer is the `Submitted` fact the books
//! already wait for.
//!
//! # What an answer means (SR-120)
//!
//! | Answer | Recorded | Why |
//! |---|---|---|
//! | `2xx` | accepted, with the optional `reference`; then booked | the recipient's platform took it |
//! | `401`, `408`, `429`, `5xx`, no answer | nothing — retried | a secret being rotated, a system starting, a system down |
//! | any other `4xx` | rejected, with the body as reason | the system read it and said no; the number stays spent |
//!
//! # What is sent (SR-119)
//!
//! A `de.emob.invoice.issued` `CloudEvent` ([`event`]) whose `data` is the
//! page's own file bytes ([`crate::invoice_page::file`]) as `application/xml`,
//! subject `invoice/<number>`, the page link as the `invoiceurl` extension —
//! signed under Standard Webhooks at the attempt's instant with `webhook-id` =
//! the invoice number, the receiver's idempotency key, because a number is
//! issued once.
//!
//! # Subscribers
//!
//! The same event goes to every `[[subscribers]]` entry that takes it
//! ([`Subscribers`]), each signed with its own secret. Whom a document is owed
//! to is fixed when it is issued; an answer is the `announced` fact. Only the
//! operator system's answer moves a document to accepted and booked.

use std::sync::{Arc, Mutex, PoisonError};

use emob_billing::Syntax;
use emob_service::Actor;
use emob_service::cloudevent::{self, CloudEvent};
use emob_service::estate::InvoiceSyntax;
use emob_service::estate::Subscription;
use emob_service::journal::Journal;
use emob_service::webhook::{self, ID_HEADER, SIGNATURE_HEADER, TIMESTAMP_HEADER};

use crate::Books;
use crate::invoice_page::{Invoices, file};

/// The extension attribute carrying the page's absolute link, where the
/// estate states one.
pub const PAGE_EXTENSION: &str = "invoiceurl";

/// The `de.emob.invoice.issued` event for one document, encoded.
///
/// # Errors
///
/// The envelope could not be encoded.
pub fn event(number: &str, file: &str, page: Option<&str>) -> serde_json::Result<String> {
    let mut event = CloudEvent::new(
        number,
        "billd",
        emob_service::events::invoice::ISSUED,
        file.to_owned(),
    )
    .about(format!("invoice/{number}"))
    .content_type("application/xml");
    if let Some(page) = page {
        event = event.with_extension(PAGE_EXTENSION, page);
    }
    serde_json::to_string(&event)
}

async fn post(
    client: &reqwest::Client,
    url: &str,
    secret: &webhook::Secret,
    number: &str,
    body: &str,
    at: time::OffsetDateTime,
) -> Result<reqwest::Response, String> {
    let signature = webhook::sign(&webhook::Delivery::new(number, at, body.as_bytes()), secret);
    client
        .post(url)
        .header(ID_HEADER, number)
        .header(TIMESTAMP_HEADER, at.unix_timestamp().to_string())
        .header(SIGNATURE_HEADER, signature)
        .header("content-type", cloudevent::CONTENT_TYPE)
        .body(body.to_owned())
        .send()
        .await
        .map_err(|error| error.to_string())
}

/// The most of a refusal's body kept as its reason (SR-121).
pub const REASON_LIMIT: usize = 512;

/// The most of an accepting reference kept (SR-121).
pub const REFERENCE_LIMIT: usize = 256;

/// Who answered: the operator system, which the estate configures.
const ANSWERED_BY: &str = "erp";

/// Why a delivery target is refused at start.
#[derive(Debug, thiserror::Error)]
#[error("billing.delivery.url {url:?} must be https, or http on a loopback host (SR-121)")]
pub struct InsecureTarget {
    url: String,
}

/// Where invoices go, how, and with which secret.
pub struct Dispatcher {
    client: reqwest::Client,
    url: String,
    secret: webhook::Secret,
    syntax: Syntax,
}

impl core::fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Dispatcher")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// What one attempt came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    /// Taken, with the reference the system returned.
    Accepted(Option<String>),
    /// Refused; why.
    Refused(String),
    /// Try again later; why not now.
    Again(String),
}

/// Whether a target may be posted to: `https`, or `http` on loopback.
#[must_use]
pub fn secure(url: &str) -> bool {
    webhook::secure_target(url)
}

fn bounded(text: &str, limit: usize) -> String {
    let mut out: String = text.chars().take(limit).collect();
    if text.chars().count() > limit {
        out.push('…');
    }
    out
}

impl Dispatcher {
    /// A dispatcher to one target.
    ///
    /// # Errors
    ///
    /// [`InsecureTarget`] for a URL that is neither `https` nor loopback
    /// `http`, or the HTTP client could not be built.
    pub fn new(
        url: &str,
        secret: webhook::Secret,
        syntax: InvoiceSyntax,
        timeout: std::time::Duration,
    ) -> anyhow::Result<Self> {
        if !secure(url) {
            return Err(InsecureTarget {
                url: url.to_owned(),
            }
            .into());
        }
        Ok(Self {
            client: reqwest::Client::builder().timeout(timeout).build()?,
            url: url.to_owned(),
            secret,
            syntax: match syntax {
                InvoiceSyntax::Ubl => Syntax::Ubl,
                InvoiceSyntax::Cii => Syntax::Cii,
            },
        })
    }

    /// Send one document once.
    pub async fn attempt(
        &self,
        number: &str,
        body: &str,
        page: Option<&str>,
        at: time::OffsetDateTime,
    ) -> Attempt {
        let body = match event(number, body, page) {
            Ok(body) => body,
            Err(error) => return Attempt::Again(error.to_string()),
        };
        let response = match post(&self.client, &self.url, &self.secret, number, &body, at).await {
            Ok(response) => response,
            Err(error) => return Attempt::Again(error),
        };
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status.is_success() {
            let reference = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("reference")?.as_str().map(str::to_owned))
                .filter(|r| !r.is_empty())
                .map(|r| bounded(&r, REFERENCE_LIMIT));
            return Attempt::Accepted(reference);
        }
        match status.as_u16() {
            401 | 408 | 429 | 500..=599 => Attempt::Again(status.to_string()),
            400..=499 => Attempt::Refused(bounded(&format!("{status}: {text}"), REASON_LIMIT)),
            _ => Attempt::Again(status.to_string()),
        }
    }

    /// Send every document still waiting once, and record each answer that
    /// ends one. Returns how many were recorded.
    pub async fn drain<J: Journal>(
        &self,
        books: &Arc<Mutex<Books<J>>>,
        invoices: &Invoices,
    ) -> usize {
        let pending: Vec<(String, Result<String, String>)> = {
            let books = books.lock().unwrap_or_else(PoisonError::into_inner);
            books
                .state()
                .documents()
                .filter(|d| d.submission == crate::Submission::Issued)
                .map(|d| {
                    (
                        d.invoice.number.clone(),
                        file(d, self.syntax).map_err(|e| e.to_string()),
                    )
                })
                .collect()
        };
        let mut recorded = 0;
        for (number, body) in pending {
            let body = match body {
                Ok(body) => body,
                Err(error) => {
                    tracing::error!(%number, %error, "an issued document could not be written");
                    continue;
                }
            };
            let now = time::OffsetDateTime::now_utc();
            let page = invoices.url(&number);
            let attempt = self.attempt(&number, &body, page.as_deref(), now).await;
            let by = Actor::service(ANSWERED_BY);
            let mut books = books.lock().unwrap_or_else(PoisonError::into_inner);
            let outcome = match attempt {
                Attempt::Accepted(reference) => books
                    .accepted(&by, &number, now.date(), reference)
                    .and_then(|()| books.book(&by, &number).map(|_| ())),
                Attempt::Refused(reason) => {
                    tracing::error!(%number, "the operator system refused an invoice — a person has to read why");
                    books.rejected(&by, &number, now.date(), reason)
                }
                Attempt::Again(why) => {
                    tracing::warn!(%number, %why, "an invoice was not handed over yet");
                    continue;
                }
            };
            match outcome {
                Ok(()) => recorded += 1,
                Err(error) => tracing::error!(%number, %error, "the answer could not be recorded"),
            }
        }
        recorded
    }
}

/// The subscribers outside the estate that take `de.emob.invoice.issued`.
pub struct Subscribers {
    client: reqwest::Client,
    list: Vec<Subscription>,
    syntax: Syntax,
}

impl core::fmt::Debug for Subscribers {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list()
            .entries(self.list.iter().map(|s| &s.name))
            .finish()
    }
}

impl Subscribers {
    /// The subscribers that take invoices, in the syntax the estate writes.
    ///
    /// # Errors
    ///
    /// The HTTP client could not be built.
    pub fn new(
        subscriptions: Vec<Subscription>,
        syntax: InvoiceSyntax,
        timeout: std::time::Duration,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder().timeout(timeout).build()?,
            list: subscriptions
                .into_iter()
                .filter(|s| s.takes(emob_service::events::invoice::ISSUED))
                .collect(),
            syntax: match syntax {
                InvoiceSyntax::Ubl => Syntax::Ubl,
                InvoiceSyntax::Cii => Syntax::Cii,
            },
        })
    }

    /// Their names, which every document issued from now on is owed to.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.list.iter().map(|s| s.name.clone()).collect()
    }

    /// Send every event still owed once, and record each answer that ends
    /// one. Returns how many were recorded.
    pub async fn drain<J: Journal>(
        &self,
        books: &Arc<Mutex<Books<J>>>,
        invoices: &Invoices,
    ) -> usize {
        let owed: Vec<(String, String, Result<String, String>)> = {
            let books = books.lock().unwrap_or_else(PoisonError::into_inner);
            books
                .state()
                .documents()
                .flat_map(|d| {
                    let file = file(d, self.syntax).map_err(|e| e.to_string());
                    d.unannounced()
                        .map(move |to| (d.invoice.number.clone(), to.to_owned(), file.clone()))
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        let mut recorded = 0;
        for (number, to, file) in owed {
            let Some(subscriber) = self.list.iter().find(|s| s.name == to) else {
                continue; // no longer in the estate: stays owed
            };
            let body = match file.and_then(|file| {
                event(&number, &file, invoices.url(&number).as_deref()).map_err(|e| e.to_string())
            }) {
                Ok(body) => body,
                Err(error) => {
                    tracing::error!(%number, %error, "an issued document could not be written");
                    continue;
                }
            };
            let now = time::OffsetDateTime::now_utc();
            let refused = match post(
                &self.client,
                &subscriber.url,
                &subscriber.secret,
                &number,
                &body,
                now,
            )
            .await
            {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        None
                    } else if matches!(status.as_u16(), 401 | 408 | 429 | 500..=599)
                        || !status.is_client_error()
                    {
                        tracing::warn!(%number, %to, %status, "an invoice event was not taken yet");
                        continue;
                    } else {
                        let text = response.text().await.unwrap_or_default();
                        tracing::error!(%number, %to, "a subscriber refused an invoice event — a person has to read why");
                        Some(bounded(&format!("{status}: {text}"), REASON_LIMIT))
                    }
                }
                Err(why) => {
                    tracing::warn!(%number, %to, %why, "an invoice event was not taken yet");
                    continue;
                }
            };
            let Ok(by) = Actor::subscriber(&to) else {
                continue;
            };
            let mut books = books.lock().unwrap_or_else(PoisonError::into_inner);
            match books.announced(&by, &number, &to, refused) {
                Ok(()) => recorded += 1,
                Err(error) => tracing::error!(%number, %error, "the answer could not be recorded"),
            }
        }
        recorded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sr_121_only_https_or_loopback_http_is_a_target() {
        assert!(secure("https://erp.example/invoices"));
        assert!(secure("http://127.0.0.1:9000/in"));
        assert!(secure("http://localhost/in"));
        assert!(secure("http://[::1]:8080/"));
        assert!(!secure("http://erp.example/in"));
        assert!(!secure("http://127.0.0.1.evil.example/"));
        assert!(!secure("ftp://erp.example/"));
    }

    #[test]
    fn sr_121_a_reason_is_bounded() {
        let long = "x".repeat(2000);
        assert_eq!(
            bounded(&long, REASON_LIMIT).chars().count(),
            REASON_LIMIT + 1
        );
        assert_eq!(bounded("short", REASON_LIMIT), "short");
    }
}
