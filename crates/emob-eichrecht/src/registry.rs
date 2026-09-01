//! The public-key registry: which key belongs to which signing component.
//!
//! # Why this is a separate thing
//!
//! A signature proves that *some* holder of *some* private key produced the
//! payload. Turning that into "this charge point produced it" needs a binding
//! between the key and the component, and OCMF is explicit that the binding
//! travels **out of band**:
//!
//! > The public keys to the charging point must be transmitted to the
//! > verification component by means other than this protocol (out-of-band) in
//! > conjunction with the ID or serial numbers used, e.g. via a central
//! > register.
//! >
//! > `[OCMF §Relation of Serial Numbers, Charge Point and Public Key]`
//!
//! A verifier that takes the key from the record it is checking has verified
//! nothing at all. This registry is therefore populated from somewhere else —
//! a station's type approval documents, the operator's provisioning system —
//! and never from a record.
//!
//! # Identification rules
//!
//! The specification defines which serial identifies the signing component:
//!
//! - the meter's serial, when the meter itself signs;
//! - the gateway's serial, when a gateway signs for a single charge point;
//! - **both**, when one gateway serves more than one charge point — because
//!   then neither alone is unique;
//! - or the charge point's own id, as an alternative.
//!
//! [`ComponentRef`] models exactly those cases, so a fleet with shared gateways
//! cannot silently collapse two charge points into one key.

use std::collections::BTreeMap;

use crate::error::VerifyError;
use crate::ocmf::{OcmfRecord, PublicKey};

/// How a signing component is identified.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ComponentRef {
    /// The meter signs, and its serial identifies it.
    Meter {
        /// The meter's serial number.
        serial: String,
    },
    /// A gateway signs for one charge point.
    Gateway {
        /// The gateway's serial number.
        serial: String,
    },
    /// A gateway serves several charge points, so both serials are needed.
    GatewayAndMeter {
        /// The gateway's serial number.
        gateway: String,
        /// The meter's serial number.
        meter: String,
    },
    /// The charge point identifies itself directly (`CT`/`CI`).
    ChargePoint {
        /// The identification type, e.g. `EVSEID`.
        id_type: String,
        /// The identification itself.
        id: String,
    },
}

impl core::fmt::Display for ComponentRef {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Meter { serial } => write!(f, "meter:{serial}"),
            Self::Gateway { serial } => write!(f, "gateway:{serial}"),
            Self::GatewayAndMeter { gateway, meter } => {
                write!(f, "gateway:{gateway}/meter:{meter}")
            }
            Self::ChargePoint { id_type, id } => write!(f, "{id_type}:{id}"),
        }
    }
}

/// A key, and the window in which it is the component's key.
///
/// A station whose signing key is replaced — a repair, a firmware change with a
/// new key pair — has two keys over its life, and a record from before the swap
/// must still verify years later. Without windows the registry can hold only
/// the current key, and every historical session becomes unverifiable the day
/// a meter is exchanged. That is the same failure that makes an invoice
/// undefendable under `[MessEG §33]`, arriving by a different route.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RegisteredKey {
    /// The key material.
    pub key: PublicKey,
    /// First instant this key is valid for, **inclusive**, if bounded.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339::option"))]
    pub valid_from: Option<time::OffsetDateTime>,
    /// The instant this key stops being valid, **exclusive**, if bounded.
    ///
    /// Half-open on purpose. With two inclusive bounds, a meter exchanged at
    /// midnight has two keys covering that instant and the registry answers
    /// with whichever was inserted first — so the same session verifies or does
    /// not depending on insertion order. `[from, until)` makes consecutive
    /// windows partition the timeline exactly, which is what a key history is.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339::option"))]
    pub valid_until: Option<time::OffsetDateTime>,
    /// Where this binding came from — a type approval, a provisioning run.
    /// Free text, and part of the evidence: a key nobody can say the origin of
    /// is a key nobody should verify against.
    pub provenance: String,
}

impl RegisteredKey {
    /// A key with no validity bounds.
    #[must_use]
    pub fn unbounded(key: PublicKey, provenance: impl Into<String>) -> Self {
        Self {
            key,
            valid_from: None,
            valid_until: None,
            provenance: provenance.into(),
        }
    }

    /// Whether this key was the component's key at `at` — `[from, until)`.
    #[must_use]
    pub fn covers(&self, at: time::OffsetDateTime) -> bool {
        self.valid_from.is_none_or(|from| at >= from)
            && self.valid_until.is_none_or(|until| at < until)
    }

    /// Whether this key's window shares an instant with another's.
    ///
    /// Two half-open intervals overlap when each starts before the other ends,
    /// with an absent bound reading as infinity in its own direction.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        let starts_before_other_ends = match (self.valid_from, other.valid_until) {
            (Some(from), Some(until)) => from < until,
            _ => true,
        };
        let other_starts_before_this_ends = match (other.valid_from, self.valid_until) {
            (Some(from), Some(until)) => from < until,
            _ => true,
        };
        starts_before_other_ends && other_starts_before_this_ends
    }
}

/// An in-memory registry of signing components and their keys.
///
/// Pure data: loading it from a database or a provisioning API is a service's
/// job, so this crate stays free of I/O and a whole fleet's verification runs
/// as a unit test.
#[derive(Debug, Clone, Default)]
pub struct KeyRegistry {
    keys: BTreeMap<ComponentRef, Vec<RegisteredKey>>,
}

impl KeyRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a key for a component.
    ///
    /// Several keys may be registered for one component; they are distinguished
    /// by their validity windows, and the windows must not overlap.
    ///
    /// # Why an overlap is refused rather than resolved
    ///
    /// [`RegisteredKey::valid_until`] is exclusive so that consecutive windows
    /// **partition** the timeline — that is the whole reason the bound is
    /// half-open. Two windows covering one instant put that guarantee back
    /// where it started: [`Self::key_at`] would answer with whichever key was
    /// inserted first, so the same session verifies or does not depending on
    /// the order a provisioning run happened to load the registry in. A
    /// verification that depends on load order is not a verification, and
    /// `[MessEG §33]` gives a customer years to ask for it again.
    ///
    /// A key that is genuinely being replaced has the old one's window closed
    /// at the swap, which is a fact the operator has and the registry cannot
    /// invent.
    ///
    /// # Errors
    ///
    /// [`RegistryError::OverlappingWindows`] when the component already holds a
    /// key whose window shares an instant with this one.
    pub fn insert(
        &mut self,
        component: ComponentRef,
        key: RegisteredKey,
    ) -> Result<(), RegistryError> {
        let name = component.to_string();
        let existing = self.keys.entry(component).or_default();
        if let Some(clash) = existing.iter().find(|held| held.overlaps(&key)) {
            return Err(RegistryError::OverlappingWindows {
                component: name,
                held: window_of(clash),
                offered: window_of(&key),
            });
        }
        existing.push(key);
        // Ascending by start, so `key_at` walks a partition rather than a bag.
        existing.sort_by_key(|held| held.valid_from);
        Ok(())
    }

    /// The key that was valid for a component at an instant.
    ///
    /// At most one can be: [`Self::insert`] refuses an overlap, so the windows
    /// partition the timeline.
    #[must_use]
    pub fn key_at(
        &self,
        component: &ComponentRef,
        at: time::OffsetDateTime,
    ) -> Option<&RegisteredKey> {
        self.keys.get(component)?.iter().find(|k| k.covers(at))
    }

    /// How many components are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Work out how a record identifies its signing component, and find its key.
    ///
    /// Tries the identifications in the order the specification gives them,
    /// most specific first: a gateway+meter pair, then a bare meter serial,
    /// then a bare gateway serial, then the charge point's own id.
    ///
    /// # Errors
    ///
    /// - [`VerifyError::NoSigningComponent`] when the record names none.
    /// - [`VerifyError::NoKeyForComponent`] when nothing is registered.
    pub fn key_for_record(
        &self,
        record: &OcmfRecord,
        at: time::OffsetDateTime,
    ) -> Result<&RegisteredKey, VerifyError> {
        let payload = &record.payload;
        let mut candidates: Vec<ComponentRef> = Vec::new();

        if let (Some(gateway), Some(meter)) = (&payload.gateway_serial, &payload.meter_serial) {
            candidates.push(ComponentRef::GatewayAndMeter {
                gateway: gateway.clone(),
                meter: meter.clone(),
            });
        }
        if let Some(meter) = &payload.meter_serial {
            candidates.push(ComponentRef::Meter {
                serial: meter.clone(),
            });
        }
        if let Some(gateway) = &payload.gateway_serial {
            candidates.push(ComponentRef::Gateway {
                serial: gateway.clone(),
            });
        }
        if let (Some(id_type), Some(id)) = (&payload.charge_point_id_type, &payload.charge_point_id)
        {
            candidates.push(ComponentRef::ChargePoint {
                id_type: id_type.clone(),
                id: id.clone(),
            });
        }

        if candidates.is_empty() {
            return Err(VerifyError::NoSigningComponent);
        }

        for candidate in &candidates {
            if let Some(key) = self.key_at(candidate, at) {
                return Ok(key);
            }
        }

        Err(VerifyError::NoKeyForComponent {
            serial: candidates
                .first()
                .map(ToString::to_string)
                .unwrap_or_default(),
        })
    }
}

/// A validity window, rendered for an error message.
fn window_of(key: &RegisteredKey) -> String {
    let bound =
        |at: Option<time::OffsetDateTime>| at.map_or_else(|| "…".to_owned(), |at| at.to_string());
    format!("[{}, {})", bound(key.valid_from), bound(key.valid_until))
}

/// What can be wrong with a registration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// Two keys for one component claim the same instant.
    #[error(
        "{component} already holds a key valid over {held}, which overlaps {offered}: \
         a record inside both would verify against whichever was registered first"
    )]
    OverlappingWindows {
        /// Which signing component.
        component: String,
        /// The window already registered.
        held: String,
        /// The window offered.
        offered: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocmf::{self, KeyType};
    use time::macros::datetime;

    fn key(byte: u8) -> PublicKey {
        PublicKey {
            algorithm: KeyType::Secp256r1,
            bytes: vec![byte; 65],
        }
    }

    fn record_with(gateway: Option<&str>, meter: Option<&str>) -> OcmfRecord {
        let mut fields = vec![r#""PG":"T1""#.to_owned()];
        if let Some(g) = gateway {
            fields.push(format!(r#""GS":"{g}""#));
        }
        if let Some(m) = meter {
            fields.push(format!(r#""MS":"{m}""#));
        }
        let raw = format!(
            r#"OCMF|{{{},"RD":[{{"TM":"2026-01-02T10:00:00,000+0100 S","RV":1,"RI":"01-00:B2.08.00*FF","RU":"kWh","ST":"G"}}]}}|{{"SD":"00"}}"#,
            fields.join(",")
        );
        ocmf::parse(&raw).unwrap()
    }

    #[test]
    fn a_meter_serial_finds_its_key() {
        let mut registry = KeyRegistry::new();
        registry
            .insert(
                ComponentRef::Meter {
                    serial: "BQ1".into(),
                },
                RegisteredKey::unbounded(key(1), "type approval 2026-01"),
            )
            .unwrap();
        let record = record_with(None, Some("BQ1"));
        let found = registry
            .key_for_record(&record, datetime!(2026-01-02 10:00 +1))
            .unwrap();
        assert_eq!(found.key, key(1));
        assert_eq!(found.provenance, "type approval 2026-01");
    }

    #[test]
    fn a_shared_gateway_needs_both_serials() {
        // One gateway, two meters: registering per gateway alone would give
        // both charge points the same key and make either able to sign for the
        // other.
        let mut registry = KeyRegistry::new();
        registry
            .insert(
                ComponentRef::GatewayAndMeter {
                    gateway: "GW1".into(),
                    meter: "M1".into(),
                },
                RegisteredKey::unbounded(key(1), "provisioning"),
            )
            .unwrap();
        registry
            .insert(
                ComponentRef::GatewayAndMeter {
                    gateway: "GW1".into(),
                    meter: "M2".into(),
                },
                RegisteredKey::unbounded(key(2), "provisioning"),
            )
            .unwrap();

        let at = datetime!(2026-01-02 10:00 +1);
        assert_eq!(
            registry
                .key_for_record(&record_with(Some("GW1"), Some("M1")), at)
                .unwrap()
                .key,
            key(1)
        );
        assert_eq!(
            registry
                .key_for_record(&record_with(Some("GW1"), Some("M2")), at)
                .unwrap()
                .key,
            key(2)
        );
    }

    #[test]
    fn a_key_swap_keeps_old_sessions_verifiable() {
        // The failure this prevents: a meter is exchanged in June, and every
        // session from January becomes unverifiable — which is the same as
        // being unbillable under MessEG §33, arriving late.
        let mut registry = KeyRegistry::new();
        let component = ComponentRef::Meter {
            serial: "BQ1".into(),
        };
        registry
            .insert(
                component.clone(),
                RegisteredKey {
                    key: key(1),
                    valid_from: None,
                    valid_until: Some(datetime!(2026-06-01 0:00 UTC)),
                    provenance: "original".into(),
                },
            )
            .unwrap();
        registry
            .insert(
                component.clone(),
                RegisteredKey {
                    key: key(2),
                    valid_from: Some(datetime!(2026-06-01 0:00 UTC)),
                    valid_until: None,
                    provenance: "after the exchange".into(),
                },
            )
            .unwrap();

        assert_eq!(
            registry
                .key_at(&component, datetime!(2026-01-15 12:00 UTC))
                .unwrap()
                .key,
            key(1)
        );
        assert_eq!(
            registry
                .key_at(&component, datetime!(2026-09-15 12:00 UTC))
                .unwrap()
                .key,
            key(2)
        );

        // The windows are half-open, so the swap instant belongs to exactly one
        // of them and the answer does not depend on insertion order.
        assert_eq!(
            registry
                .key_at(&component, datetime!(2026-06-01 0:00 UTC))
                .unwrap()
                .key,
            key(2)
        );
    }

    #[test]
    fn a_gap_between_windows_is_a_gap_rather_than_a_guess() {
        // A component whose key history has a hole in it must fail to resolve
        // in the hole, not silently fall through to a neighbouring key.
        let mut registry = KeyRegistry::new();
        let component = ComponentRef::Meter {
            serial: "BQ1".into(),
        };
        registry
            .insert(
                component.clone(),
                RegisteredKey {
                    key: key(1),
                    valid_from: None,
                    valid_until: Some(datetime!(2026-06-01 0:00 UTC)),
                    provenance: "original".into(),
                },
            )
            .unwrap();
        registry
            .insert(
                component.clone(),
                RegisteredKey {
                    key: key(2),
                    valid_from: Some(datetime!(2026-07-01 0:00 UTC)),
                    valid_until: None,
                    provenance: "after the exchange".into(),
                },
            )
            .unwrap();

        assert!(
            registry
                .key_at(&component, datetime!(2026-06-15 12:00 UTC))
                .is_none(),
            "June has no registered key, and inventing one is how a forged record verifies"
        );
    }

    #[test]
    fn an_unregistered_component_says_so() {
        let registry = KeyRegistry::new();
        let record = record_with(None, Some("UNKNOWN"));
        assert!(matches!(
            registry.key_for_record(&record, datetime!(2026-01-02 10:00 +1)),
            Err(VerifyError::NoKeyForComponent { .. })
        ));
    }

    #[test]
    fn a_record_naming_no_component_cannot_be_verified() {
        let registry = KeyRegistry::new();
        let record = record_with(None, None);
        assert!(matches!(
            registry.key_for_record(&record, datetime!(2026-01-02 10:00 +1)),
            Err(VerifyError::NoSigningComponent)
        ));
    }

    #[test]
    fn two_keys_may_not_claim_one_instant() {
        // The reason `valid_until` is exclusive is that consecutive windows
        // partition the timeline. Two windows over one instant put that back
        // where it started: `key_at` would answer with whichever was loaded
        // first, so the same record verifies or does not depending on the order
        // a provisioning run happened to run in.
        let component = ComponentRef::Meter {
            serial: "BQ1".into(),
        };
        let mut registry = KeyRegistry::new();
        registry
            .insert(
                component.clone(),
                RegisteredKey {
                    key: key(1),
                    valid_from: None,
                    valid_until: Some(datetime!(2026-06-01 00:00 UTC)),
                    provenance: "type approval".into(),
                },
            )
            .unwrap();

        // The replacement starting a day early overlaps by a day.
        let err = registry
            .insert(
                component.clone(),
                RegisteredKey {
                    key: key(2),
                    valid_from: Some(datetime!(2026-05-31 00:00 UTC)),
                    valid_until: None,
                    provenance: "meter exchange".into(),
                },
            )
            .unwrap_err();
        assert!(matches!(err, RegistryError::OverlappingWindows { .. }));
        assert!(err.to_string().contains("registered first"));

        // …and starting exactly where the first one stops does not, because the
        // bound is exclusive.
        registry
            .insert(
                component.clone(),
                RegisteredKey {
                    key: key(2),
                    valid_from: Some(datetime!(2026-06-01 00:00 UTC)),
                    valid_until: None,
                    provenance: "meter exchange".into(),
                },
            )
            .unwrap();
        assert_eq!(
            registry
                .key_at(&component, datetime!(2026-06-01 00:00 UTC))
                .unwrap()
                .key,
            key(2)
        );
        assert_eq!(
            registry
                .key_at(&component, datetime!(2026-05-31 23:59 UTC))
                .unwrap()
                .key,
            key(1)
        );
    }

    #[test]
    fn an_unbounded_key_leaves_room_for_nothing_else() {
        // An unbounded window covers every instant, so a second key for the
        // same component is always an overlap — which is the honest answer: a
        // registry holding two keys with no dates for one meter cannot say
        // which signed a record.
        let component = ComponentRef::Meter {
            serial: "BQ1".into(),
        };
        let mut registry = KeyRegistry::new();
        registry
            .insert(component.clone(), RegisteredKey::unbounded(key(1), "a"))
            .unwrap();
        assert!(
            registry
                .insert(component, RegisteredKey::unbounded(key(2), "b"))
                .is_err()
        );
    }
}
