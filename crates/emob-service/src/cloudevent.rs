//! The `CloudEvents` 1.0 envelope of every event emob sends.
//!
//! One shape for every receiver — `billd` and `roamd` inside the estate, an
//! ERP, a field-service adapter or a data warehouse outside it — so a receiver
//! implements one reader. Structured mode (`application/cloudevents+json`):
//! the whole envelope is the body, so the Standard Webhooks signature over the
//! body covers `type`, `subject` and `time` too, which binary mode's
//! `ce-*` headers would leave unsigned.
//!
//! ```
//! use emob_service::cloudevent::CloudEvent;
//! use emob_service::events;
//!
//! let at = time::macros::datetime!(2026-11-19 12:00 UTC);
//! let event = CloudEvent::new("DE*ABC*E0001*1", "csmsd", events::cdr::ISSUED, 42)
//!     .about("cdr/DE*ABC*E0001*1")
//!     .at(at);
//! let json = serde_json::to_value(&event)?;
//! assert_eq!(json["specversion"], "1.0");
//! assert_eq!(json["source"], "urn:emob:csmsd");
//! assert_eq!(json["type"], "de.emob.cdr.issued");
//! assert_eq!(json["time"], "2026-11-19T12:00:00Z");
//! # Ok::<(), serde_json::Error>(())
//! ```

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The `CloudEvents` version every envelope states.
pub const SPEC_VERSION: &str = "1.0";

/// The media type a structured-mode event is posted as.
pub const CONTENT_TYPE: &str = "application/cloudevents+json";

/// The `source` of an event a daemon emits: `urn:emob:<daemon>`.
///
/// `CloudEvents` §3.1.1 requires a URI-reference, which a bare name is not.
#[must_use]
pub fn source(daemon: &str) -> String {
    format!("urn:emob:{daemon}")
}

/// One event, as it goes on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudEvent<D> {
    /// Always [`SPEC_VERSION`].
    pub specversion: String,
    /// Unique per source; the receiver's idempotency key, and the `webhook-id`.
    pub id: String,
    /// The emitting daemon, from [`source`].
    pub source: String,
    /// A type from [`crate::events`].
    #[serde(rename = "type")]
    pub kind: String,
    /// What it is about, as `<entity>/<id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// When the fact happened, where the emitter knows the instant.
    #[serde(with = "time::serde::rfc3339::option")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<time::OffsetDateTime>,
    /// The media type of `data`.
    pub datacontenttype: String,
    /// The fact. A document that is not JSON (an XML invoice) is a string.
    pub data: D,
    /// Extension attributes, flat beside the core ones (`CloudEvents` §3.3),
    /// so a router reads one without parsing `data`.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl<D> CloudEvent<D> {
    /// An event a daemon emits, with JSON data.
    #[must_use]
    pub fn new(id: impl Into<String>, daemon: &str, kind: &'static str, data: D) -> Self {
        Self {
            specversion: SPEC_VERSION.to_owned(),
            id: id.into(),
            source: source(daemon),
            kind: kind.to_owned(),
            subject: None,
            time: None,
            datacontenttype: "application/json".to_owned(),
            data,
            extensions: BTreeMap::new(),
        }
    }

    /// What the event is about, as `<entity>/<id>`.
    #[must_use]
    pub fn about(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// When the fact happened.
    #[must_use]
    pub const fn at(mut self, instant: time::OffsetDateTime) -> Self {
        self.time = Some(instant);
        self
    }

    /// Data in another media type than JSON.
    #[must_use]
    pub fn content_type(mut self, media_type: &str) -> Self {
        media_type.clone_into(&mut self.datacontenttype);
        self
    }

    /// An extension attribute. Its name is lowercase ASCII letters and digits
    /// (`CloudEvents` §3.1.2).
    #[must_use]
    pub fn with_extension(mut self, name: &'static str, value: impl Into<String>) -> Self {
        debug_assert!(
            name.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
            "{name}"
        );
        self.extensions
            .insert(name.to_owned(), serde_json::Value::String(value.into()));
        self
    }
}

/// Why a body is not the event a receiver takes.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Refusal {
    /// Not a structured-mode `CloudEvent` with this data.
    #[error("not a CloudEvent this receiver reads: {0}")]
    Malformed(String),
    /// Another `specversion`.
    #[error("specversion {0} is not {SPEC_VERSION}")]
    Version(String),
    /// Another type than the one this endpoint takes.
    #[error("this endpoint takes {expected}, not {found}")]
    Type {
        /// What it takes.
        expected: &'static str,
        /// What arrived.
        found: String,
    },
}

/// Read a signed CDR addressed to either estate receiver.
///
/// A conforming dry run returns `None` and never reaches settlement. Call after verifying the
/// signature and freshness of the raw body.
///
/// # Errors
///
/// Refuses an unexpected source, type, delivery ID or malformed event.
pub fn open_cdr<D: DeserializeOwned>(body: &[u8], delivery_id: &str) -> Result<Option<D>, Refusal> {
    let event = open::<Box<serde_json::value::RawValue>>(body, crate::events::cdr::ISSUED)?;
    if event.source != "urn:emob:csmsd" || event.id != delivery_id {
        return Err(Refusal::Malformed(
            "unexpected CDR source or event id".into(),
        ));
    }
    match event.extensions.get("emobtest") {
        Some(serde_json::Value::Bool(true)) => Ok(None),
        None | Some(serde_json::Value::Bool(false)) => serde_json::from_str(event.data.get())
            .map(Some)
            .map_err(|error| Refusal::Malformed(error.to_string())),
        Some(_) => Err(Refusal::Malformed("emobtest must be a boolean".into())),
    }
}

/// Read a body as an event of exactly one type.
///
/// # Errors
///
/// A [`Refusal`] naming what is wrong.
pub fn open<D: DeserializeOwned>(
    body: &[u8],
    expected: &'static str,
) -> Result<CloudEvent<D>, Refusal> {
    let event: CloudEvent<D> =
        serde_json::from_slice(body).map_err(|error| Refusal::Malformed(error.to_string()))?;
    if event.specversion != SPEC_VERSION {
        return Err(Refusal::Version(event.specversion));
    }
    if event.kind != expected {
        return Err(Refusal::Type {
            expected,
            found: event.kind,
        });
    }
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_sample_still_has_to_identify_its_source_type_and_delivery() {
        let sample = |daemon, kind| {
            let mut event = CloudEvent::new("sample-1", daemon, kind, ());
            event.extensions.insert("emobtest".into(), true.into());
            serde_json::to_vec(&event).unwrap()
        };
        let body = sample("csmsd", events::cdr::ISSUED);
        assert!(open_cdr::<String>(&body, "sample-1").unwrap().is_none());
        assert!(open_cdr::<String>(&body, "another").is_err());
        assert!(open_cdr::<String>(&sample("billd", events::cdr::ISSUED), "sample-1").is_err());
        assert!(open_cdr::<String>(&sample("csmsd", events::invoice::ISSUED), "sample-1").is_err());
    }

    use crate::events;

    fn at() -> time::OffsetDateTime {
        time::macros::datetime!(2026-11-19 12:00 UTC)
    }

    #[test]
    fn an_event_reads_back_as_it_was_written() {
        let event = CloudEvent::new(
            "A-1",
            "billd",
            events::invoice::ISSUED,
            "<Invoice/>".to_owned(),
        )
        .about("invoice/A-1")
        .at(at())
        .content_type("application/xml")
        .with_extension("invoiceurl", "https://billd.example/i/x");
        let bytes = serde_json::to_vec(&event).unwrap();
        let read: CloudEvent<String> = open(&bytes, events::invoice::ISSUED).unwrap();
        assert_eq!(read, event);
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json["invoiceurl"], "https://billd.example/i/x",
            "extensions are flat"
        );
    }

    #[test]
    fn another_type_is_refused_by_name() {
        let event = CloudEvent::new("A-1", "billd", events::invoice::ISSUED, 1);
        let bytes = serde_json::to_vec(&event).unwrap();
        assert_eq!(
            open::<u8>(&bytes, events::cdr::ISSUED),
            Err(Refusal::Type {
                expected: events::cdr::ISSUED,
                found: events::invoice::ISSUED.to_owned()
            })
        );
    }

    #[test]
    fn a_bare_body_is_not_an_event() {
        assert!(matches!(
            open::<serde_json::Value>(br#"{"cdr":{}}"#, events::cdr::ISSUED),
            Err(Refusal::Malformed(_))
        ));
    }

    #[test]
    fn another_spec_version_is_refused() {
        let mut event = CloudEvent::new("A-1", "csmsd", events::cdr::ISSUED, 1);
        event.specversion = "0.3".to_owned();
        let bytes = serde_json::to_vec(&event).unwrap();
        assert_eq!(
            open::<u8>(&bytes, events::cdr::ISSUED),
            Err(Refusal::Version("0.3".to_owned()))
        );
    }
}
