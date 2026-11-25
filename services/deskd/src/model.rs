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
    /// Currently registered stations, including those which have never charged.
    pub station_count: usize,
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
    /// Each civil day in the owner's reporting zone, oldest first.
    pub days: Vec<Daily>,
    /// Today.
    pub today: Window,
    /// The seven days ending today.
    pub week: Window,
}

/// One of the seven civil-day totals.
#[derive(Debug, Clone, Deserialize)]
pub struct Daily {
    /// Reporting date as YYYY-MM-DD.
    pub from: String,
    /// The billed/attempted counts.
    pub estate: Rate,
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
    /// Stable outcome position within this estate.
    pub position: u64,
    /// Additional findings beyond the owner's display limit.
    #[serde(default)]
    pub omitted_findings: usize,
    /// Additional diagnostic reasons beyond the owner's display limit.
    #[serde(default)]
    pub omitted_reasons: usize,
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
    /// `14:30` when the silent point is held for a driver.
    #[serde(default)]
    pub reserved_until: Option<String>,
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

/// Direct dynamic-offer receipt and cumulative delivery evidence from poid.
#[derive(Debug, Clone, Deserialize)]
pub struct Publication {
    /// Static table delivery attempts including retries, when counted.
    pub static_attempts: Option<u64>,
    /// Start of the latest static table attempt.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub static_last_attempt: Option<time::OffsetDateTime>,
    /// Most recent direct static table acknowledgement.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub static_accepted_at: Option<time::OffsetDateTime>,
    /// Last acknowledged complete snapshot.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub snapshot_at: Option<time::OffsetDateTime>,
    /// Last acknowledged dynamic delivery, whether snapshot or delta.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub accepted_at: Option<time::OffsetDateTime>,
    /// Oldest observed change still awaiting receiver acknowledgement.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub pending_since: Option<time::OffsetDateTime>,
    /// Oldest changed source fact in the most recent measured delivery.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_source_change: Option<time::OffsetDateTime>,
    /// Receiver acknowledgement paired with that source fact.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_measured_at: Option<time::OffsetDateTime>,
    /// Dynamic delivery attempts including retries, when the publisher counts them.
    pub attempts: Option<u64>,
    /// Start of the last dynamic attempt, independent of its outcome.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub last_attempt: Option<time::OffsetDateTime>,
    /// Delta messages acknowledged since the last complete snapshot.
    pub changes: usize,
    /// Deliveries with at least one new source change older than sixty seconds.
    pub missed_deadlines: u64,
    /// The publisher's latest source or delivery problem.
    pub problem: Option<String>,
}
