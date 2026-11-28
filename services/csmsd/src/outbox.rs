//! Handing each `de.emob.cdr.issued` event to `billd`, `roamd` and every
//! subscriber that takes it, until each has it.
//!
//! The outbox is part of `csmsd`'s journal: a record settles and the
//! deliveries it owes are one fact, so no restart can leave a session billed
//! here and owed nowhere. This module only **dispatches**: a signed `POST` of
//! the `CloudEvent` per pending delivery — with the estate's secret inside it,
//! a subscriber's own outside it — and a fact for each answer that ends one
//! (SR-031).
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
use emob_service::estate::Subscription;
use emob_service::journal::Journal;
use emob_service::webhook::{self, transport};
use transport::Disposition;

use crate::Csmsd;
use crate::journal::{Delivery, Event, Target};

/// Where deliveries go, and the secrets they are signed with.
pub struct Dispatcher {
    client: transport::Client,
    secret: webhook::Secret,
    billd: String,
    roamd: String,
    subscribers: Vec<Subscription>,
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
            client: transport::Client::new(timeout)?,
            secret,
            billd: billd.trim_end_matches('/').to_owned(),
            roamd: roamd.trim_end_matches('/').to_owned(),
            subscribers: Vec::new(),
        })
    }

    /// Deliver to these subscribers as well.
    #[must_use]
    pub fn with_subscribers(mut self, subscribers: Vec<Subscription>) -> Self {
        self.subscribers = subscribers;
        self
    }

    /// Where a delivery goes and what it is signed with; `None` for a
    /// subscriber the estate no longer names — it stays owed, and is sent if
    /// the subscriber comes back.
    fn route(&self, to: &Target) -> Option<(String, &webhook::Secret)> {
        match to {
            Target::Billd => Some((format!("{}/v1/deliveries", self.billd), &self.secret)),
            Target::Roamd => Some((format!("{}/v1/deliveries", self.roamd), &self.secret)),
            Target::Subscriber(name) => self
                .subscribers
                .iter()
                .find(|s| &s.name == name && s.takes(emob_service::events::cdr::ISSUED))
                .map(|s| (s.url.clone(), &s.secret)),
        }
    }

    /// Send one delivery once.
    pub async fn attempt(&self, delivery: &Delivery, at: time::OffsetDateTime) -> Attempt {
        let Some((url, secret)) = self.route(&delivery.to) else {
            return Attempt::Again(format!(
                "the estate names no subscriber {}",
                delivery.to.name()
            ));
        };
        let reply = match self
            .client
            .post(&url, secret, &delivery.id, delivery.body.clone(), at)
            .await
        {
            Ok(reply) => reply,
            Err(error) => return Attempt::Again(error.to_string()),
        };
        match reply.disposition() {
            Disposition::Accepted => Attempt::Delivered,
            Disposition::Refused => Attempt::Refused(format!("{}: {}", reply.status, reply.text)),
            Disposition::Retry => Attempt::Again(reply.status.to_string()),
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
                    to: delivery.to.clone(),
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
                        to: delivery.to.clone(),
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
            let by = match &delivery.to {
                Target::Billd => Actor::service("billd"),
                Target::Roamd => Actor::service("roamd"),
                Target::Subscriber(name) => match Actor::subscriber(name) {
                    Ok(actor) => actor,
                    Err(error) => {
                        tracing::error!(%error, "a subscriber's name is not an actor");
                        continue;
                    }
                },
            };
            if let Err(error) = csmsd.record(&by, fact) {
                tracing::error!(?error, id = %delivery.id, "the answer could not be recorded");
            }
        }
    }
}

/// The delivery as it is **sent**: every delivery but `roamd`'s gains `data.receipt`, the
/// absolute link to the driver's receipt, which the invoice line carries
/// (spec 003).
///
/// Added here and not when the delivery was journalled, so `csmsd`'s journal
/// never holds a receipt token (SR-062). The link is a function of the record's
/// key, so every attempt still sends the same bytes.
#[must_use]
pub fn with_receipt<J: Journal>(csmsd: &Csmsd<J>, delivery: &Delivery) -> Delivery {
    if delivery.to == Target::Roamd {
        return delivery.clone();
    }
    let Some(url) = csmsd.receipt_url_for_record(&delivery.id) else {
        return delivery.clone();
    };
    let Ok(mut event) = serde_json::from_str::<serde_json::Value>(&delivery.body) else {
        return delivery.clone();
    };
    let Some(data) = event
        .get_mut("data")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return delivery.clone();
    };
    data.insert("receipt".to_owned(), serde_json::Value::String(url));
    Delivery {
        body: event.to_string(),
        ..delivery.clone()
    }
}
