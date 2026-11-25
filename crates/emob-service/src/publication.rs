//! The bounded public-data projection read by the national-access-point publisher.

use crate::estate::Location;
use emob_core::{ConnectorType, EvseId};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// An observed connector notification. A missing source time cannot establish latency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorObservation {
    /// OCPP connector state.
    pub status: String,
    /// Time reported by the station.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// Public fields only. Operator notes, credentials and driver identifiers stay in the CSMS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicPoint {
    /// Operator declaration that the electricity supplied is 100% renewable.
    pub fully_renewable: Option<bool>,
    /// OCPP identity serving this single point.
    pub identity: String,
    /// The EVSE identifier.
    pub evse: EvseId,
    /// Rated output.
    pub rated_power_kw: Decimal,
    /// Connector type.
    pub connector: ConnectorType,
    /// Public site details.
    pub location: Location,
    /// Tariff assigned by the registry.
    pub tariff: String,
    /// Monotone facility revision.
    pub version: u32,
    /// Revision of the containing site in the registry.
    pub site_version: u32,
    /// When this registration last changed.
    #[serde(with = "time::serde::rfc3339")]
    pub changed_at: OffsetDateTime,
    /// Time the source last changed the point's effective availability, including a transition to unknown.
    #[serde(with = "time::serde::rfc3339::option", default)]
    pub status_changed_at: Option<OffsetDateTime>,
    /// Most recent timestamped report from a connected station.
    pub observation: Option<ConnectorObservation>,
}

/// A page from one revision of the CSMS register.
#[derive(Debug, Serialize, Deserialize)]
pub struct RegistryPage {
    /// Changes whenever a feed-relevant fact changes.
    pub revision: u64,
    /// Public charge points ordered by station identity.
    pub points: Vec<PublicPoint>,
    /// Offset of the next page of this same revision.
    pub next: Option<usize>,
}
