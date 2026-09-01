//! Which station is which, and which key is its.
//!
//! # Why a station is not allowed to tell you either
//!
//! Two bindings decide whether a session can be billed, and OCPP is the wrong
//! channel for both:
//!
//! - **Identity → charge point.** The `Identity` in a WebSocket URL is whatever
//!   the station was configured with. Accepting an unknown one and inventing an
//!   EVSE id for it produces sessions attributed to a point nobody provisioned.
//!   `[OCPP 2.0.1 Part 4 §3.1.1]` has an answer for this — **404** — and
//!   [`Provisioning`] is what makes that answer possible.
//! - **Component → public key.** OCMF is explicit that the key "must be
//!   transmitted to the verification component by means other than this
//!   protocol (out-of-band)" `[OCMF §Relation of Serial Numbers]`. A station
//!   sends its own `publicKey` beside every signed value and offers a
//!   `MeterPublicKey` configuration key `[OCA SMV §3.3.1]`; neither is a
//!   binding, and a CSMS that trusted either would verify every record against
//!   whichever key made it verify.
//!
//! So both come from here — a type approval, a provisioning run, an operator's
//! own database — and never from the socket.

use std::collections::BTreeMap;

use emob_core::EvseId;
use ocpp_kit::types::Identity;

/// One provisioned charge point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChargePoint {
    /// The point this station is.
    pub evse_id: EvseId,
    /// What it can deliver, in kW.
    ///
    /// Carried because `[AFIR Art. 5(4)]` turns on it: at 50 kW and above the
    /// ad-hoc price must be based on a price per kWh, and only there may an
    /// occupancy fee be added.
    pub rated_power_kw: rust_decimal::Decimal,
}

/// The fleet a CSMS will accept.
#[derive(Debug, Clone, Default)]
pub struct Provisioning {
    points: BTreeMap<Identity, ChargePoint>,
}

impl Provisioning {
    /// An empty fleet, which accepts nobody.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Provision one station.
    #[must_use]
    pub fn with(mut self, identity: Identity, point: ChargePoint) -> Self {
        self.points.insert(identity, point);
        self
    }

    /// The point a station is, if it was provisioned.
    #[must_use]
    pub fn get(&self, identity: &Identity) -> Option<&ChargePoint> {
        self.points.get(identity)
    }

    /// Whether this identity may connect at all.
    ///
    /// The question `[OCPP 2.0.1 Part 4 §3.1.1]` wants answered with a **404**
    /// rather than a 401, so an operator can tell a typo from a bad password.
    #[must_use]
    pub fn knows(&self, identity: &Identity) -> bool {
        self.points.contains_key(identity)
    }

    /// How many points are provisioned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Whether the fleet is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point() -> ChargePoint {
        ChargePoint {
            evse_id: "DE*ABC*E00001".parse().unwrap(),
            rated_power_kw: rust_decimal::Decimal::from(150),
        }
    }

    #[test]
    fn an_unprovisioned_station_is_unknown_rather_than_invented() {
        // Inventing an EVSE id for a station nobody provisioned produces
        // sessions attributed to a point that does not exist.
        let fleet = Provisioning::new().with(Identity::new("CP-1").unwrap(), point());

        assert!(fleet.knows(&Identity::new("CP-1").unwrap()));
        assert!(!fleet.knows(&Identity::new("CP-2").unwrap()));
        assert_eq!(fleet.get(&Identity::new("CP-2").unwrap()), None);
        assert_eq!(fleet.len(), 1);
    }
}
