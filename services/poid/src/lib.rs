//! `poid` — the service that publishes an operator's charge points.
//!
//! # What it decides, and what it must not
//!
//! `[AFIR Art. 20(2)]` makes an operator's static and dynamic point data
//! available free of charge through the national access point, and
//! `[AFIR Art. 20(3)]` makes the API registered there free and unrestricted.
//! In Germany the format is not a choice: from 14.04.2026 the feed speaks the
//! **DATEX II Recharging profile** `[DATEX-II-Profil]`.
//!
//! The documents are [`emob_poi`]'s. What is here is the half a domain crate
//! cannot have: *when* a snapshot goes out, *which* dynamic updates have
//! accumulated since the last one, and *whether the access point took it*. The
//! service composes; it does not describe.
//!
//! # A snapshot is a claim about now, so it is refused before it is sent
//!
//! [`Feed::check`] is not a formality and it runs before every publication:
//! a station whose power claim contradicts its own points, and — the half that
//! is otherwise silent — a **rate published at a site on a different clock**. A
//! `22:00` night price written in `Europe/Berlin` and published at a site in
//! `Europe/Lisbon` is a well-formed document, a lawful tariff, a real site, and
//! a price that starts an hour after the driver standing there thinks it does.
//! Nothing fails until somebody compares a bill against a map.
//!
//! # The dynamic half references the static half at a version
//!
//! A status message addresses a facility **at the version the table published
//! it at**, and `[DATEX-II-Profil]` gives a consumer no way to resolve a
//! reference to a version it never received. So a status publication that
//! preceded its own table is a document every consumer silently drops — which is
//! why [`Poid::status`] refuses to produce one before a snapshot has been
//! accepted, rather than sending it and hoping.
//!
//! # No I/O
//!
//! Nothing here opens a socket. [`Poid::snapshot`] and [`Poid::status`] return
//! the documents and the daemon sends them; [`Poid::accepted`] records a push
//! the access point **took**. A push that failed leaves the feed unconfirmed,
//! so [`Poid::stale`] reports it rather than the service forgetting — the same
//! separation `tarifd` keeps between attempting a delivery and recording one.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::pedantic)]

use emob_poi::datex::{
    InformationStatus, PointUpdate, Publisher, StatusPublication, TablePublication,
};
use emob_poi::site::{ChargingPoint, Facility, Site};
use emob_poi::{Feed, PoiError, Rate};

/// What a national access point was last told, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accepted {
    /// When the publication this service built was written.
    pub published_at: time::OffsetDateTime,
    /// When the access point accepted it.
    pub at: time::OffsetDateTime,
}

/// Why a publication could not be produced.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PublishError {
    /// The inventory contradicts itself, or a price is published on the wrong
    /// clock.
    #[error(transparent)]
    Feed(#[from] PoiError),

    /// A dynamic update was asked for before any snapshot was accepted.
    ///
    /// A status message addresses a facility at the version the **table**
    /// published it at, and a consumer that never received that table cannot
    /// resolve the reference. `[DATEX-II-Profil]` gives it no way to ask, so
    /// the message is dropped in silence — a charger that reads `available`
    /// here and is missing from every map, with nothing failing anywhere.
    #[error(
        "no snapshot has been accepted yet, so a status message would reference facility versions \
         no consumer has: publish the table first [DATEX-II-Profil]"
    )]
    NoSnapshotYet,
}

/// A feed that is in force and that the access point has not been told about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stale {
    /// When the last accepted snapshot was written, if there has ever been one.
    pub last_accepted: Option<time::OffsetDateTime>,
    /// How long the feed has gone unconfirmed.
    pub overdue_by: time::Duration,
}

impl core::fmt::Display for Stale {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.last_accepted {
            Some(at) => write!(
                f,
                "the national access point last accepted a snapshot at {at}, {} minutes ago: \
                 `[AFIR Art. 20(2)]` makes this data an operator's duty to publish, and a feed \
                 nobody refreshed is one route planners are reading as current",
                self.overdue_by.whole_minutes()
            ),
            None => write!(
                f,
                "the national access point has never accepted a snapshot: every point this \
                 operator runs is absent from the feed `[AFIR Art. 20(2)]`, and no dynamic update \
                 can reference a facility version nobody has published"
            ),
        }
    }
}

/// The service: an operator's inventory, the prices its points publish, and
/// what the national access point has taken.
pub struct Poid<'a> {
    /// Who publishes, and under what national identifier.
    pub publisher: Publisher,
    /// Whether this describes reality. A production feed published as `test` is
    /// invisible; a test feed published as `real` sends drivers to a fiction.
    pub information_status: InformationStatus,
    /// The table's own identity, which every status message references.
    pub table: Facility,
    /// A name for it, when the operator groups its estate.
    pub table_name: Option<String>,
    /// The infrastructure.
    pub sites: Vec<Site>,
    /// The published rate for a point, when it has one.
    ///
    /// A closure rather than a field on the point, for [`Feed`]'s own reason:
    /// prices change on a different clock from concrete, and the same point may
    /// be published under different rates to different access points. In a
    /// deployment this reads what `tarifd` published.
    pub rate_for: &'a dyn Fn(&ChargingPoint) -> Option<Rate>,
    /// What the access point last accepted.
    accepted: Option<Accepted>,
}

impl<'a> Poid<'a> {
    /// A service holding an inventory that has never been published.
    #[must_use]
    pub fn new(
        publisher: Publisher,
        table: Facility,
        sites: Vec<Site>,
        rate_for: &'a dyn Fn(&ChargingPoint) -> Option<Rate>,
    ) -> Self {
        Self {
            publisher,
            information_status: InformationStatus::Real,
            table,
            table_name: None,
            sites,
            rate_for,
            accepted: None,
        }
    }

    /// The feed this service publishes, assembled from its own inventory.
    ///
    /// One value, so the references in a status message address facilities at
    /// the versions the table publishes them at. There is no second inventory
    /// to drift from — which is the whole reason [`Feed`] takes both halves.
    fn feed(&self) -> Feed<'_> {
        Feed {
            publisher: self.publisher.clone(),
            information_status: self.information_status,
            table: self.table.clone(),
            table_name: self.table_name.clone(),
            sites: self.sites.clone(),
            rate_for: self.rate_for,
        }
    }

    /// The static publication — `[AFIR Art. 20(2)(a)–(b)]`.
    ///
    /// `published_at` is an argument rather than a clock, because a snapshot
    /// replayed two years later has to produce the same bytes: a national
    /// access point keeps what it was sent, and an operator asked to show what
    /// it published has to be able to produce it again.
    ///
    /// # Errors
    ///
    /// [`PublishError::Feed`] when the inventory contradicts itself, or a rate
    /// is published at a site on a different clock. A feed is not published
    /// before its own contradictions are.
    pub fn snapshot(
        &self,
        published_at: time::OffsetDateTime,
    ) -> Result<TablePublication, PublishError> {
        Ok(self.feed().table(published_at)?)
    }

    /// The dynamic publication — `[AFIR Art. 20(2)(c)]`.
    ///
    /// # Errors
    ///
    /// [`PublishError::NoSnapshotYet`] before any table has been accepted — see
    /// the type for why that is a refusal rather than a warning — and
    /// [`PublishError::Feed`] when an update addresses something the table does
    /// not carry at that version.
    pub fn status(
        &self,
        updates: &[PointUpdate],
        published_at: time::OffsetDateTime,
    ) -> Result<StatusPublication, PublishError> {
        if self.accepted.is_none() {
            return Err(PublishError::NoSnapshotYet);
        }
        Ok(self.feed().status(updates, published_at)?)
    }

    /// Record a snapshot the access point **took**.
    ///
    /// Called after the push, never before it. A push that failed leaves the
    /// feed unconfirmed and [`Self::stale`] reports it, which is the whole
    /// reason recording a delivery is a separate act from attempting one.
    pub const fn accepted(&mut self, published_at: time::OffsetDateTime, at: time::OffsetDateTime) {
        self.accepted = Some(Accepted { published_at, at });
    }

    /// What the access point last took.
    #[must_use]
    pub const fn last_accepted(&self) -> Option<Accepted> {
        self.accepted
    }

    /// Whether the feed has gone unrefreshed for longer than `within`.
    ///
    /// `None` while it is current. A feed nobody refreshed is one route
    /// planners and comparison sites are reading as though it were, which is
    /// the failure mode of published data: nothing errors, and the map is
    /// simply wrong.
    #[must_use]
    pub fn stale(&self, now: time::OffsetDateTime, within: time::Duration) -> Option<Stale> {
        match self.accepted {
            None => Some(Stale {
                last_accepted: None,
                overdue_by: time::Duration::ZERO,
            }),
            Some(accepted) => {
                let age = now - accepted.at;
                (age > within).then_some(Stale {
                    last_accepted: Some(accepted.published_at),
                    overdue_by: age - within,
                })
            }
        }
    }
}
