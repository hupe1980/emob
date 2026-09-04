//! What a slice of a charging session was, and therefore what may price it.

/// What a slice of a session **was** — and therefore which time dimension, if
/// any, may price it.
///
/// # Two questions, not one
///
/// "Was energy flowing" and "may this minute carry an occupancy fee" look like
/// the same question. A vehicle that has stopped asking for power is loitering
/// on a post somebody else wants; a vehicle still asking for power that the
/// *operator* is not offering — a charging profile at zero, a `[EnWG §14a]`
/// curtailment, a grid limit, a fault — is doing nothing wrong, and no energy is
/// flowing there either.
///
/// `[OCPI 2.3.0 §mod_cdrs_chargingperiod_class]` defines `PARKING_TIME` on the
/// vehicle's own demand — "time during which the **vehicle is not requesting
/// power**" — and says why it is not "time not charging":
///
/// > Under that definition, drivers would be exposed to penalizing loitering
/// > fees not only when they leave their vehicle in a charging session after it
/// > has been fully charged, **but also when the EVSE is not offering energy to
/// > the vehicle while the vehicle is still requesting power**.
///
/// So a boolean cannot carry it, and this is the third state:
///
/// | | `TIME` | `PARKING_TIME` | energy transferred |
/// |---|---|---|---|
/// | [`Self::Charging`] | ✅ | | ✅ |
/// | [`Self::Parked`] | | ✅ | |
/// | [`Self::Withheld`] | | | |
///
/// The mapping to a dimension is `emob_tariff::Dimension::pricing`, beside the
/// dimensions, because this crate has never heard of a tariff.
///
/// [`Self::Withheld`] is priced by **neither** — OCPI has only "time charging"
/// and "time the vehicle was not requesting power". It still belongs to the
/// session: inside `total_time`, and inside the `total_parking_time` a CDR
/// reports, which is defined on *energy transfer* rather than on demand
/// `[OCPI 2.3.0 §mod_cdrs_cdr_object]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Activity {
    /// Energy was flowing. Priced as charging time.
    ///
    /// A period at zero energy that is still marked charging is a taper, not an
    /// occupancy: the vehicle was asking and the point was offering.
    #[default]
    Charging,
    /// The **vehicle** was not requesting power — the battery is full, or the
    /// car paused its own charge. Priced as occupancy.
    ///
    /// This is the occupancy `[AFIR Art. 5(4)]` lets a fast charger price per
    /// minute, and the only thing that is.
    Parked,
    /// The **operator** was not offering power while the vehicle was still
    /// requesting it — a charging profile at zero, a `[EnWG §14a]` dimming, a
    /// grid limit, a fault. Priced by nothing.
    Withheld,
}

impl Activity {
    /// This crate's own name for the state — the spelling [`Display`] writes
    /// and the one `serde` uses.
    ///
    /// **Not an OCPP value.** `[OCPP 2.1 ChargingStateEnumType]` spells its
    /// states `Charging`, `EVConnected`, `SuspendedEV`, `SuspendedEVSE` and
    /// `Idle`, and none of the three below is one of them — a station sent any
    /// of these would reject the message. The mapping in both directions lives
    /// at the seam that has the protocol in scope: `emob_ocpp::kit::activity_from`
    /// reads a `chargingState` into an [`Activity`], and
    /// `emob_ocpp::transaction` writes one back out.
    ///
    /// This crate has never heard of OCPP, and the doc comment that said
    /// otherwise was an invitation to put the wrong literal on a wire (D252).
    ///
    /// [`Display`]: core::fmt::Display
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Charging => "charging",
            Self::Parked => "parked",
            Self::Withheld => "withheld",
        }
    }

    /// Whether energy moved across the meter here.
    ///
    /// The question `total_parking_time` is defined on, which is **not** the
    /// question the priced `PARKING_TIME` dimension is defined on — see the
    /// type's own documentation for why the specification carries both.
    #[must_use]
    pub const fn transfers_energy(self) -> bool {
        matches!(self, Self::Charging)
    }
}

impl core::fmt::Display for Activity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}
