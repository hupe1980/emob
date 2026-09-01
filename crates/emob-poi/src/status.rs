//! What a point is doing now, and what the register says it is allowed to say.
//!
//! `[AFIR Art. 20(2)(c)]` makes operational status and availability **dynamic**
//! data: an operator publishes it, free of charge, through the national access
//! point, and route planners act on it within seconds. It is the half of the
//! feed that is wrong most often, because it is the half that changes.
//!
//! # The register is upstream of the feed
//!
//! `[LSV26 §4(1)]` makes three things notifiable — commissioning,
//! decommissioning and a change of operator — so an operator that meets its
//! notification duty necessarily *knows*, for every point, which of three
//! states it is in. `[DATEX-II-Profil Tab. B.45]` has a literal for two of
//! them: `planned` for a point that is not operating yet, `removed` for one
//! that has been discontinued.
//!
//! Those two facts are usually held in different systems, and the feed is
//! usually generated from the one that does not know. The result is the
//! commonest defect in European charging data: a point that was decommissioned
//! months ago, still published as `available`, still routed to. [`Lifecycle`]
//! and [`PointStatus`] are joined here so that document cannot be built.

use crate::error::{PoiError, Result};

/// Where a point is in the `[LSV26 §4]` register.
///
/// Three states, because the Verordnung names three notifiable events. A change
/// of operator is not a state — the point keeps operating throughout — which is
/// why it is a notice in [`emob_core::Registration`] and not a variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Lifecycle {
    /// Built, or being built, and not yet commissioned.
    ///
    /// Nothing has been notified yet, because `[LSV26 §4(1) Nr. 1]` runs its
    /// deadline from commissioning and not from construction — the 2016
    /// Verordnung ran it four weeks *before* Errichtung, and the 2026 one does
    /// not.
    Planned,
    /// Commissioned and in service.
    #[default]
    Operating,
    /// Taken out of service `[LSV26 §4(1) Nr. 2]`.
    Decommissioned,
}

impl Lifecycle {
    /// A short stable name, for a log line or an error.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Operating => "operating",
            Self::Decommissioned => "decommissioned",
        }
    }

    /// The only status a point in this state may publish, when there is one.
    ///
    /// `Operating` returns `None`: an operating point's status is a live fact
    /// that the register has nothing to say about. The other two are entirely
    /// determined, which is the point.
    #[must_use]
    pub const fn forced_status(self) -> Option<PointStatus> {
        match self {
            Self::Planned => Some(PointStatus::Planned),
            Self::Operating => None,
            Self::Decommissioned => Some(PointStatus::Removed),
        }
    }
}

/// What a point is doing, in the profile's own vocabulary.
///
/// All twelve literals of `[DATEX-II-Profil Tab. B.45]`, because dropping the
/// ones that seem redundant is how a feed ends up reporting `outOfOrder` for a
/// blocked bay and sending a service technician to a working charger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PointStatus {
    /// Not occupied, energy available, usable.
    Available,
    /// Physically unreachable — a car parked across it.
    Blocked,
    /// In use, charging.
    Charging,
    /// Faulty.
    Faulted,
    /// Not yet active, or no longer available.
    Inoperative,
    /// In use; may or may not be charging.
    Occupied,
    /// Out of order.
    OutOfOrder,
    /// No energy available, short term.
    OutOfStock,
    /// Planned, will operate soon.
    Planned,
    /// Discontinued or removed.
    Removed,
    /// Reserved for a customer.
    Reserved,
    /// No energy available, longer term.
    Unavailable,
}

impl PointStatus {
    /// The spelling `[DATEX-II-Profil Tab. B.45]` uses.
    #[must_use]
    pub const fn as_profile_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Blocked => "blocked",
            Self::Charging => "charging",
            Self::Faulted => "faulted",
            Self::Inoperative => "inoperative",
            Self::Occupied => "occupied",
            Self::OutOfOrder => "outOfOrder",
            Self::OutOfStock => "outOfStock",
            Self::Planned => "planned",
            Self::Removed => "removed",
            Self::Reserved => "reserved",
            Self::Unavailable => "unavailable",
        }
    }

    /// Whether this status would send a driver here.
    ///
    /// The question a route planner asks, and the reason a wrong status costs
    /// somebody a detour rather than a log line.
    #[must_use]
    pub const fn invites_a_driver(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// A point's live status, checked against what the register permits.
///
/// Construct one with [`Report::new`]; there is no other way to make one, and
/// that is the whole design. A `Report` in hand is a status that the register
/// agrees with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    lifecycle: Lifecycle,
    status: PointStatus,
}

impl Report {
    /// A status a point in this lifecycle state may publish.
    ///
    /// # Errors
    ///
    /// [`PoiError::StatusContradictsRegister`] when the register says the point
    /// is not operating and the status says otherwise. The failure this
    /// prevents is not a malformed document — the profile would accept
    /// `available` for a point removed last spring — it is a driver arriving at
    /// a concrete pad.
    pub fn new(point: &str, lifecycle: Lifecycle, status: PointStatus) -> Result<Self> {
        match lifecycle.forced_status() {
            Some(required) if required != status => Err(PoiError::StatusContradictsRegister {
                point: point.to_owned(),
                lifecycle: lifecycle.as_str(),
                status: status.as_profile_str(),
            }),
            _ => Ok(Self { lifecycle, status }),
        }
    }

    /// The status of an operating point, whatever it is doing.
    #[must_use]
    pub const fn operating(status: PointStatus) -> Self {
        Self {
            lifecycle: Lifecycle::Operating,
            status,
        }
    }

    /// What the register says.
    #[must_use]
    pub const fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }

    /// What the feed will say.
    #[must_use]
    pub const fn status(&self) -> PointStatus {
        self.status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decommissioned_point_cannot_be_published_as_available() {
        // The commonest defect in European charging data, and the one a schema
        // validator has nothing to say about.
        let refused = Report::new(
            "DE*ABC*E00001",
            Lifecycle::Decommissioned,
            PointStatus::Available,
        );
        assert!(matches!(
            refused,
            Err(PoiError::StatusContradictsRegister { .. })
        ));

        // The register's own answer is admissible, and it is the only one.
        assert!(
            Report::new(
                "DE*ABC*E00001",
                Lifecycle::Decommissioned,
                PointStatus::Removed
            )
            .is_ok()
        );
    }

    #[test]
    fn a_planned_point_is_planned_and_not_merely_unavailable() {
        // `unavailable` and `outOfOrder` both read as "come back later" to a
        // route planner. `planned` reads as "this does not exist yet", which is
        // the true statement and the one a map should draw differently.
        assert!(matches!(
            Report::new("p", Lifecycle::Planned, PointStatus::Unavailable),
            Err(PoiError::StatusContradictsRegister { .. })
        ));
        assert!(Report::new("p", Lifecycle::Planned, PointStatus::Planned).is_ok());
    }

    #[test]
    fn an_operating_point_may_say_anything_because_the_register_does_not_know() {
        for status in [
            PointStatus::Available,
            PointStatus::Charging,
            PointStatus::Faulted,
            PointStatus::OutOfOrder,
            PointStatus::Reserved,
        ] {
            assert!(
                Report::new("p", Lifecycle::Operating, status).is_ok(),
                "{status:?}"
            );
        }
    }

    #[test]
    fn only_available_invites_a_driver() {
        // `occupied` and `charging` mean the point works and is busy; a planner
        // may still route there and wait. `available` is the only one that
        // promises a free socket now.
        assert!(PointStatus::Available.invites_a_driver());
        for busy in [
            PointStatus::Charging,
            PointStatus::Occupied,
            PointStatus::Reserved,
            PointStatus::Blocked,
            PointStatus::Removed,
        ] {
            assert!(!busy.invites_a_driver(), "{busy:?}");
        }
    }
}
