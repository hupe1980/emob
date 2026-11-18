//! Handing a settled record to `billd` and `roamd`, until each has it.
//!
//! The outbox is part of `csmsd`'s journal: a record settles and the two
//! deliveries it owes are one fact, so no restart can leave a session billed
//! here and owed nowhere. This module only **dispatches**: a signed `POST` per
//! pending delivery, and a fact for each answer that ends one (SR-031).
//!
//! # What an answer means
//!
//! | Answer | Recorded | Why |
//! |---|---|---|
//! | `2xx` | delivered | the receiver journalled it before it answered |
//! | `401`, `408`, `429`, `5xx`, no answer | nothing — retried | a secret being rotated, a receiver starting, a receiver down |
//! | any other `4xx` | refused, with the body | the receiver read it and said no; a retry would say no again, and a person has to read why |
//!
//! Each attempt is signed at the instant it is sent: receivers refuse a
//! signature older than their tolerance, so replaying an hour-old attempt
//! byte for byte would be refused as the replay it looks like.

use emob_service::Actor;
use emob_service::journal::Journal;
use emob_service::webhook::{self, ID_HEADER, SIGNATURE_HEADER, TIMESTAMP_HEADER};

use crate::Csmsd;
use crate::journal::{Delivery, Event, Target};

/// Where deliveries go, and the secret they are signed with.
pub struct Dispatcher {
    client: reqwest::Client,
    secret: webhook::Secret,
    billd: String,
    roamd: String,
}

impl core::fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Dispatcher")
            .field("billd", &self.billd)
            .field("roamd", &self.roamd)
            .finish_non_exhaustive()
    }
}

/// What one attempt came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    /// The receiver has it.
    Delivered,
    /// The receiver will not take it; why.
    Refused(String),
    /// Try again later; why not now.
    Again(String),
}

impl Dispatcher {
    /// A dispatcher to the two receivers.
    ///
    /// # Errors
    ///
    /// The HTTP client could not be built.
    pub fn new(
        billd: &str,
        roamd: &str,
        secret: webhook::Secret,
        timeout: std::time::Duration,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder().timeout(timeout).build()?,
            secret,
            billd: billd.trim_end_matches('/').to_owned(),
            roamd: roamd.trim_end_matches('/').to_owned(),
        })
    }

    fn url(&self, to: Target) -> String {
        let base = match to {
            Target::Billd => &self.billd,
            Target::Roamd => &self.roamd,
        };
        format!("{base}/v1/deliveries")
    }

    /// Send one delivery once.
    pub async fn attempt(&self, delivery: &Delivery, at: time::OffsetDateTime) -> Attempt {
        let signature = webhook::sign(
            &webhook::Delivery::new(&delivery.id, at, delivery.body.as_bytes()),
            &self.secret,
        );
        let sent = self
            .client
            .post(self.url(delivery.to))
            .header(ID_HEADER, &delivery.id)
            .header(TIMESTAMP_HEADER, at.unix_timestamp().to_string())
            .header(SIGNATURE_HEADER, signature)
            .header("content-type", "application/json")
            .body(delivery.body.clone())
            .send()
            .await;
        let response = match sent {
            Ok(response) => response,
            Err(error) => return Attempt::Again(error.to_string()),
        };
        let status = response.status();
        if status.is_success() {
            return Attempt::Delivered;
        }
        let text = response.text().await.unwrap_or_default();
        match status.as_u16() {
            401 | 408 | 429 | 500..=599 => Attempt::Again(format!("{status}: {text}")),
            400..=499 => Attempt::Refused(format!("{status}: {text}")),
            _ => Attempt::Again(format!("{status}: {text}")),
        }
    }

    /// Send every pending delivery once, and record each answer that ends one.
    pub async fn drain<J: Journal>(&self, csmsd: &Csmsd<J>) {
        let pending: Vec<Delivery> = csmsd.with_state(|state| state.pending().cloned().collect());
        for delivery in pending {
            let at = time::OffsetDateTime::now_utc();
            let sent = with_receipt(csmsd, &delivery);
            let fact = match self.attempt(&sent, at).await {
                Attempt::Delivered => Event::Delivered {
                    id: delivery.id.clone(),
                    to: delivery.to,
                    at,
                },
                Attempt::Refused(reason) => {
                    tracing::error!(
                        id = %delivery.id,
                        to = delivery.to.name(),
                        %reason,
                        "the receiver refused the record — a person has to read this, not a retry"
                    );
                    Event::Refused {
                        id: delivery.id.clone(),
                        to: delivery.to,
                        reason,
                        at,
                    }
                }
                Attempt::Again(why) => {
                    tracing::warn!(id = %delivery.id, to = delivery.to.name(), %why, "not delivered yet");
                    continue;
                }
            };
            // The receiver whose answer decided it is who caused the fact.
            let by = match delivery.to {
                Target::Billd => Actor::service("billd"),
                Target::Roamd => Actor::service("roamd"),
            };
            if let Err(error) = csmsd.record(&by, fact) {
                tracing::error!(?error, id = %delivery.id, "the answer could not be recorded");
            }
        }
    }
}

/// The delivery as it is **sent**: a `billd` delivery gains `"receipt"`, the
/// absolute link to the driver's receipt, which the invoice line carries
/// (spec 003).
///
/// Added here and not when the delivery was journalled, so `csmsd`'s journal
/// never holds a receipt token (SR-062). The link is a function of the record's
/// key, so every attempt still sends the same bytes.
#[must_use]
pub fn with_receipt<J: Journal>(csmsd: &Csmsd<J>, delivery: &Delivery) -> Delivery {
    if delivery.to != Target::Billd {
        return delivery.clone();
    }
    let Some(url) = csmsd.receipt_url_for_record(&delivery.id) else {
        return delivery.clone();
    };
    let Ok(serde_json::Value::Object(mut body)) = serde_json::from_str(&delivery.body) else {
        return delivery.clone();
    };
    body.insert("receipt".to_owned(), serde_json::Value::String(url));
    Delivery {
        body: serde_json::Value::Object(body).to_string(),
        ..delivery.clone()
    }
}
