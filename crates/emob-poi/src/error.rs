//! What a register or a feed can be wrong about.

use thiserror::Error;

/// A publication that would misdescribe the infrastructure it publishes.
///
/// Every variant is something a national access point, a route planner or a
/// driver would act on. None of them is a schema violation — the DATEX II
/// profile would accept every one of these documents — which is exactly why
/// they are worth a type: a feed that validates and lies is the failure mode
/// `[AFIR Art. 20]` has no answer for.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum PoiError {
    /// A station claims a total power below the largest point it contains.
    ///
    /// `totalMaximumPower` is `1..1` in the profile `[DATEX-II-Profil Tab. 7]`,
    /// and a route planner uses it to decide whether a vehicle can be charged
    /// here at all. A station that publishes less than one of its own outlets
    /// can deliver is turning traffic away from capacity it has.
    #[error("station {station} publishes a total of {total} kW but holds a point rated {point} kW")]
    TotalPowerBelowPoint {
        /// Which station.
        station: String,
        /// The published station total, in kW.
        total: String,
        /// The largest point's rating, in kW.
        point: String,
    },

    /// A station claims a total power above the sum of its points.
    ///
    /// The opposite lie, and the one a grid connection makes tempting: the
    /// station's total is bounded above by what its outlets can actually
    /// deliver, whatever the transformer behind it is rated at.
    #[error("station {station} publishes a total of {total} kW but its points sum to {sum} kW")]
    TotalPowerAboveSum {
        /// Which station.
        station: String,
        /// The published station total, in kW.
        total: String,
        /// The sum of the points' ratings, in kW.
        sum: String,
    },

    /// A point publishes an operational status its register state forbids.
    ///
    /// `[LSV26 §4]` makes commissioning, decommissioning and an operator change
    /// notifiable events, so the register knows which of the three states a
    /// point is in. `[DATEX-II-Profil Tab. B.45]` has literals for the two
    /// non-operating ones — `planned` and `removed` — and a feed that reports a
    /// decommissioned point as `available` is sending drivers to a socket that
    /// is not there.
    #[error("point {point} is {lifecycle} in the register but published as {status}")]
    StatusContradictsRegister {
        /// Which point.
        point: String,
        /// What the register says.
        lifecycle: &'static str,
        /// What the feed would say.
        status: &'static str,
    },

    /// A status message references a facility version the table never published.
    ///
    /// The status publication addresses everything by `idG` **and** `versionG`
    /// `[DATEX-II-Profil]`. A consumer that holds version 1 of a point and
    /// receives a status for version 2 has no object to attach it to, and the
    /// standard answer is to drop it — silently. So a feed whose two halves
    /// disagree about a version goes dark without any error anywhere.
    #[error(
        "status references {facility} version {referenced}, but the table published version {published}"
    )]
    VersionNotPublished {
        /// Which facility.
        facility: String,
        /// The version the status message cites.
        referenced: String,
        /// The version the table actually published.
        published: String,
    },

    /// A status message references a facility the table does not contain.
    #[error("status references {facility}, which the table does not contain")]
    FacilityNotPublished {
        /// The dangling reference.
        facility: String,
    },

    /// A price cannot be expressed in the profile's own vocabulary.
    ///
    /// See [`crate::rate`] — `[DATEX-II-Profil Tab. A.116]` has no literal for
    /// the occupancy fee `[AFIR Art. 5(4)]` explicitly permits.
    #[error("{dimension} has no faithful price type in the profile: {because}")]
    UnpublishablePrice {
        /// The tariff dimension that has no target.
        dimension: &'static str,
        /// Why not.
        because: &'static str,
    },

    /// A rate is published at a site whose wall clock runs on a different zone.
    ///
    /// A tariff's `22:00` is local civil time at the charge point
    /// `[OCPI 2.3.0 §mod_tariffs_tariffrestrictions_class]`, and the site says
    /// which clock that is. A rate written in one zone and published at a site
    /// in another states a window the driver standing there is never inside: the
    /// feed shows a night price that starts an hour late and the session is
    /// rated at a third time.
    ///
    /// Silent in every direction but this one. The document itself is
    /// well-formed, the tariff is lawful, the site is real, and nothing fails
    /// until somebody compares a bill against a map.
    #[error(
        "the rate '{rate}' is read on the wall clock of {rate_zone} and the site '{site}' runs on {site_zone}: the published price applies at hours this site never sees"
    )]
    RateZoneIsNotTheSites {
        /// The rate, by its published id.
        rate: String,
        /// The zone its windows are read in.
        rate_zone: String,
        /// The site, by facility id.
        site: String,
        /// The zone the site runs on.
        site_zone: String,
    },
}

/// A convenience alias.
pub type Result<T> = core::result::Result<T, PoiError>;
