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

use ocmf::{PublicKey, Record};

use crate::error::KeyLookupError;

/// A public key on the wire: the curve it is on, and its SEC1 point in hex.
#[cfg(feature = "serde")]
mod key_wire {
    use ocmf::{Curve, PublicKey};
    use serde::{Deserialize, Deserializer, Serialize as _, Serializer};

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Wire {
        curve: String,
        sec1: String,
    }

    pub(super) fn serialize<S: Serializer>(
        key: &PublicKey,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        Wire {
            curve: key.curve().name().to_owned(),
            sec1: hex::encode(key.sec1_bytes()),
        }
        .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PublicKey, D::Error> {
        use serde::de::Error as _;
        let wire = Wire::deserialize(deserializer)?;
        let curve = Curve::ALL
            .into_iter()
            .find(|c| c.name() == wire.curve)
            .ok_or_else(|| D::Error::custom(format!("unknown curve {:?}", wire.curve)))?;
        let bytes = hex::decode(&wire.sec1).map_err(D::Error::custom)?;
        PublicKey::from_sec1(curve, &bytes).map_err(D::Error::custom)
    }
}

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
    ///
    /// Travels as its curve and its SEC1 point in hex — the shape a type
    /// approval publishes and the shape the transparency container carries —
    /// rather than as a derived structure, so a registry exported today is one
    /// a different build can read.
    #[cfg_attr(feature = "serde", serde(with = "key_wire"))]
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
    /// key whose window shares an instant with this one, and
    /// [`RegistryError::EmptyWindow`] when the offered window covers no instant
    /// at all.
    pub fn insert(
        &mut self,
        component: ComponentRef,
        key: RegisteredKey,
    ) -> Result<(), RegistryError> {
        let name = component.to_string();

        // A window that ends before it begins covers nothing — `covers` is
        // `from <= at < until`, which no instant satisfies when `until <= from`.
        // The overlap sweep below cannot see it either: an empty interval
        // overlaps nothing by construction, so it registers cleanly and then
        // verifies nothing, and an operator reading the registry believes the
        // component is provisioned. The same shape `TariffHistory` refuses in a
        // price history, for the same reason and with the same answer.
        if let (Some(from), Some(until)) = (key.valid_from, key.valid_until)
            && until <= from
        {
            return Err(RegistryError::EmptyWindow {
                component: name,
                window: window_of(&key),
            });
        }

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
    /// - [`KeyLookupError::NoSigningComponent`] when the record names none.
    /// - [`KeyLookupError::NoKeyForComponent`] when nothing is registered.
    /// - [`KeyLookupError::NoKeyValidAt`] when one is and its windows have closed.
    pub fn key_for_record(
        &self,
        record: &Record<'_>,
        at: time::OffsetDateTime,
    ) -> Result<&RegisteredKey, KeyLookupError> {
        let payload = record.payload();
        let meter = payload.meter_serial();
        let gateway = payload.gateway_serial();
        let mut candidates: Vec<ComponentRef> = Vec::new();

        if let (Some(gateway), Some(meter)) = (gateway, meter) {
            candidates.push(ComponentRef::GatewayAndMeter {
                gateway: gateway.to_owned(),
                meter: meter.to_owned(),
            });
        }
        if let Some(meter) = meter {
            candidates.push(ComponentRef::Meter {
                serial: meter.to_owned(),
            });
        }
        if let Some(gateway) = gateway {
            candidates.push(ComponentRef::Gateway {
                serial: gateway.to_owned(),
            });
        }
        if let (Some(id_type), Some(id)) =
            (payload.charge_point_id_type(), payload.charge_point_id())
        {
            candidates.push(ComponentRef::ChargePoint {
                id_type: id_type.as_str().to_owned(),
                id: id.to_owned(),
            });
        }

        if candidates.is_empty() {
            return Err(KeyLookupError::NoSigningComponent);
        }

        // Specificity is decided by what the registry knows about the
        // component, **not** by which entry happens to hold a live key.
        //
        // Choosing the first candidate with a *valid* key instead lets a
        // gateway-and-meter key whose window has closed fall through to a bare
        // meter key registered for the same physical meter — so which key
        // verifies a record depends on which side of a window boundary it
        // falls, which is the load-order hazard `insert` refuses one level up,
        // arriving through the clock instead. Once the component is
        // identified, an expired window is an answer the operator needs rather
        // than a reason to consult a different identity.
        let Some(component) = candidates
            .iter()
            .find(|candidate| self.keys.contains_key(candidate))
        else {
            return Err(KeyLookupError::NoKeyForComponent {
                component: candidates
                    .first()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            });
        };

        self.key_at(component, at)
            .ok_or_else(|| KeyLookupError::NoKeyValidAt {
                component: component.to_string(),
                at,
                windows: self
                    .keys
                    .get(component)
                    .map(|held| held.iter().map(window_of).collect::<Vec<_>>().join(", "))
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

    /// A key's window covers no instant.
    ///
    /// `[from, until)` with `until <= from` is empty, so the key can never be
    /// the one a record verifies against — and nothing downstream would say so:
    /// the component would simply have no key at every instant, which reads
    /// exactly like a component nobody registered.
    #[error(
        "the key offered for {component} is valid over {window}, which is no instant at all:          a half-open window ending at or before it begins can never be the key a record          verifies against, and the component would read as unprovisioned"
    )]
    EmptyWindow {
        /// Which signing component.
        component: String,
        /// The window offered.
        window: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocmf::{Curve, PublicKey};
    use time::macros::datetime;

    /// An uncompressed secp256r1 point whose only job is to be distinguishable.
    ///
    /// `0x04` is the SEC1 tag for an uncompressed point, and the registry never
    /// does arithmetic with a key — it stores and returns one. A key that has to
    /// be *usable* is signed for in `evidence`.
    fn key(byte: u8) -> PublicKey {
        let mut bytes = vec![byte; 65];
        bytes[0] = 0x04;
        PublicKey::from_sec1(Curve::Secp256r1, &bytes).expect("a well-formed point encoding")
    }

    /// The text of a record identifying itself the way the arguments say.
    fn record_text(gateway: Option<&str>, meter: Option<&str>) -> String {
        let mut fields = vec![r#""FV":"1.0","PG":"T1""#.to_owned()];
        if let Some(g) = gateway {
            fields.push(format!(r#""GS":"{g}""#));
        }
        if let Some(m) = meter {
            fields.push(format!(r#""MS":"{m}""#));
        }
        format!(
            r#"OCMF|{{{},"RD":[{{"TM":"2026-01-02T10:00:00,000+0100 S","TX":"B","RV":1,"RI":"01-00:B2.08.00*FF","RU":"kWh","EF":"","ST":"G"}}]}}|{{"SD":"00"}}"#,
            fields.join(",")
        )
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
        let text = record_text(None, Some("BQ1"));
        let record = ocmf::Record::parse(&text).unwrap();
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
                .key_for_record(
                    &ocmf::Record::parse(&record_text(Some("GW1"), Some("M1"))).unwrap(),
                    at
                )
                .unwrap()
                .key,
            key(1)
        );
        assert_eq!(
            registry
                .key_for_record(
                    &ocmf::Record::parse(&record_text(Some("GW1"), Some("M2"))).unwrap(),
                    at
                )
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
    fn an_expired_specific_key_does_not_fall_through_to_a_broader_one() {
        // The record names both serials, so the gateway-and-meter pair is the
        // identification `[OCMF §Relation of Serial Numbers]` prescribes. Its
        // key's window has closed, and the same physical meter is also
        // registered on its own — with a *different* key.
        //
        // Resolving the first candidate that happens to hold a live key would
        // make which key verifies a record depend on which side of a window
        // boundary it falls, which is the load-order hazard `insert` refuses,
        // arriving through the clock instead.
        let mut registry = KeyRegistry::new();
        registry
            .insert(
                ComponentRef::GatewayAndMeter {
                    gateway: "GW-1".into(),
                    meter: "BQ1".into(),
                },
                RegisteredKey {
                    key: key(1),
                    valid_from: None,
                    valid_until: Some(datetime!(2026-06-01 0:00 UTC)),
                    provenance: "the pair".into(),
                },
            )
            .unwrap();
        registry
            .insert(
                ComponentRef::Meter {
                    serial: "BQ1".into(),
                },
                RegisteredKey::unbounded(key(2), "the bare meter"),
            )
            .unwrap();

        let text = record_text(Some("GW-1"), Some("BQ1"));
        let record = ocmf::Record::parse(&text).unwrap();

        // Inside the window, the specific key resolves.
        let found = registry
            .key_for_record(&record, datetime!(2026-05-01 0:00 UTC))
            .unwrap();
        assert_eq!(found.provenance, "the pair");

        // Outside it, the answer is the gap — not the other key.
        let err = registry
            .key_for_record(&record, datetime!(2026-07-01 0:00 UTC))
            .unwrap_err();
        assert!(
            matches!(err, KeyLookupError::NoKeyValidAt { ref component, .. }
                if component.contains("GW-1")),
            "{err}"
        );
        assert!(
            err.to_string().contains("windows covers"),
            "the operator has to see that the key was replaced, not that none exists: {err}"
        );
    }

    #[test]
    fn an_unregistered_component_says_so() {
        let registry = KeyRegistry::new();
        let text = record_text(None, Some("UNKNOWN"));
        let record = ocmf::Record::parse(&text).unwrap();
        assert!(matches!(
            registry.key_for_record(&record, datetime!(2026-01-02 10:00 +1)),
            Err(KeyLookupError::NoKeyForComponent { .. })
        ));
    }

    #[test]
    fn a_record_naming_no_component_cannot_be_verified() {
        let registry = KeyRegistry::new();
        let text = record_text(None, None);
        let record = ocmf::Record::parse(&text).unwrap();
        assert!(matches!(
            registry.key_for_record(&record, datetime!(2026-01-02 10:00 +1)),
            Err(KeyLookupError::NoSigningComponent)
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

    #[test]
    fn a_window_that_covers_no_instant_is_refused_rather_than_stored() {
        // `[from, until)` with `until <= from` is empty, and the overlap sweep
        // cannot see it: an empty interval overlaps nothing, so it would
        // register cleanly, verify nothing, and leave an operator reading the
        // registry believing the component is provisioned. `TariffHistory`
        // refuses the same shape in a price history for the same reason.
        let component = ComponentRef::Meter {
            serial: "BQ1".into(),
        };
        let mut registry = KeyRegistry::new();

        let inverted = RegisteredKey {
            key: key(1),
            valid_from: Some(datetime!(2026-06-01 00:00 UTC)),
            valid_until: Some(datetime!(2026-01-01 00:00 UTC)),
            provenance: "a typo in a provisioning run".into(),
        };
        let err = registry
            .insert(component.clone(), inverted)
            .expect_err("an empty window");
        assert!(matches!(err, RegistryError::EmptyWindow { .. }), "{err}");
        assert!(err.to_string().contains("no instant at all"), "{err}");

        // …and one that opens and closes at the same instant is the same fault.
        let instantaneous = RegisteredKey {
            key: key(1),
            valid_from: Some(datetime!(2026-06-01 00:00 UTC)),
            valid_until: Some(datetime!(2026-06-01 00:00 UTC)),
            provenance: "a".into(),
        };
        assert!(registry.insert(component.clone(), instantaneous).is_err());

        // Nothing was stored, so the component is still open for a real key.
        assert!(
            registry
                .insert(component, RegisteredKey::unbounded(key(2), "b"))
                .is_ok()
        );
    }
}
