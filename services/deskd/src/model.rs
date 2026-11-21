//! The bounded views Heute renders, as `deskd` reads them (spec 007).
//!
//! Mirrors rather than the daemons' own types: depending on `csmsd` to read
//! its JSON would put `ocpp-kit` and `ocpi-kit` under a page renderer. Only the
//! fields the page shows are named, and unknown fields are ignored, so a daemon
//! adding to its view does not break the page. The end-to-end test reads the
//! real daemons, which is what holds the two in step.
//!
//! Where a value is a domain type — a rate, a finding — it **is** the domain
//! type, so the page cannot word a kind the domain does not have.

use emob_eichrecht::Finding;
use emob_session::success::Rate;
use serde::Deserialize;

/// One page of a list, newest first, and the cursor for the older ones.
#[derive(Debug, Clone, Deserialize)]
pub struct Page<T> {
    /// The whole list's length.
    pub total: u64,
    /// The rows on this page.
    pub items: Vec<T>,
    /// The cursor for the next page, when there is one.
    pub older: Option<u64>,
}

impl<T> Default for Page<T> {
    fn default() -> Self {
        Self {
            total: 0,
            items: Vec::new(),
            older: None,
        }
    }
}

/// `GET /v1/csmsd/today`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Csmsd {
    /// The success rate, today and over the week.
    pub success: Windows,
    /// The newest refused sessions.
    pub refused: Page<Refusal>,
    /// The silent stations.
    pub silent: Page<Station>,
    /// The mobile networks silent stations are on, the most silent first.
    #[serde(default)]
    pub networks: Vec<NetworkShare>,
}

/// One mobile network with a silent station (spec 022).
#[derive(Debug, Clone, Deserialize)]
pub struct NetworkShare {
    /// MCC-MNC.
    pub network: String,
    /// Who runs it.
    #[serde(default)]
    pub carrier: Option<String>,
    /// Its silent stations.
    pub silent: u64,
    /// Its stations.
    pub stations: u64,
}

/// The two windows.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Windows {
    /// Today.
    pub today: Window,
    /// The seven days ending today.
    pub week: Window,
}

/// The rate over a span of days.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Window {
    /// The estate.
    pub estate: Rate,
    /// The weakest points, lowest rate first.
    pub worst: Vec<Point>,
}

/// One point's rate.
#[derive(Debug, Clone, Deserialize)]
pub struct Point {
    /// Its identity.
    pub point: String,
    /// Its rate.
    pub rate: Rate,
}

/// One refused session.
#[derive(Debug, Clone, Deserialize)]
pub struct Refusal {
    /// The station.
    pub identity: String,
    /// The transaction.
    pub transaction: String,
    /// The chain's sentences.
    pub reasons: Vec<String>,
    /// The chain's typed findings.
    pub findings: Vec<Finding>,
}

/// One silent station.
#[derive(Debug, Clone, Deserialize)]
pub struct Station {
    /// Its OCPP identity.
    pub identity: String,
    /// The point it is.
    pub evse: String,
    /// Its heartbeat interval.
    pub heartbeat_secs: i32,
    /// Since when it has been silent.
    #[serde(with = "time::serde::rfc3339")]
    pub silent_since: time::OffsetDateTime,
    /// Its mobile network, MCC-MNC.
    #[serde(default)]
    pub network: Option<String>,
    /// Who runs it.
    #[serde(default)]
    pub carrier: Option<String>,
    /// Its newest note.
    #[serde(default)]
    pub note: Option<String>,
}

/// `GET /v1/billd/today`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Billd {
    /// Documents their recipient has not accepted.
    pub invoices: Page<Document>,
}

/// One document.
#[derive(Debug, Clone, Deserialize)]
pub struct Document {
    /// Its number.
    pub number: String,
    /// The gross total, as the document states it.
    pub total: String,
    /// Where it stands with its recipient.
    pub submission: Submission,
}

/// `billd`'s `Submission`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Submission {
    /// Not yet accepted.
    Issued,
    /// Taken.
    Accepted {},
    /// Refused, and why.
    Rejected {
        /// What the platform said.
        reason: String,
    },
}

/// `GET /v1/roamd/today`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Roamd {
    /// Consignments a partner has not accepted.
    pub consignments: Page<Consignment>,
}

/// One consignment.
#[derive(Debug, Clone, Deserialize)]
pub struct Consignment {
    /// The record.
    pub key: String,
    /// To whom.
    pub recipient: String,
    /// `pending` or `refused`.
    pub delivery: String,
    /// The refusal's reason.
    pub reason: Option<String>,
}
