//! The pools a contract is issued through, and the trust anchors each publishes.
//!
//! # Why this is plural, and was plural on the first day
//!
//! R10 asks *when* to add a second Plug & Charge pool, and answers it by
//! refusing the question: a [`Contract`](crate::Contract) references **a**
//! pool, never *the* pool. The failure that avoids is not a missing feature —
//! it is the shape of a platform written for one pool, where "the trust store"
//! is a global and every certificate is validated against everything in it.
//!
//! That platform violates `[V2G2-925]` by construction:
//!
//! > \[a leaf\] shall be treated as invalid, if the trust anchor at the end of
//! > the chain does not match the specific root certificate required for a
//! > certain use.
//!
//! With one pool the rule is unobservable, because there is only one anchor set
//! and every chain that validates validates against the right one. With two it
//! is the whole of the security property: a contract minted in one ecosystem
//! and presented in another **validates beautifully** — the mathematics is
//! sound, every signature checks, every profile holds — and it is a contract
//! this platform has no relationship with. Nothing fails. A session is
//! authorised, a CDR is written, and an invoice goes to a provider that never
//! heard of the driver.
//!
//! So there is no global anchor set here. [`PoolRegistry::validate_in`] takes
//! the pool by name and can only reach that pool's anchors.
//!
//! # The anchors carry windows, and the windows are enforced
//!
//! The same arrangement `emob_eichrecht::KeyRegistry` makes for a station's
//! key, and for the same reason: an anchor is configuration, it is replaced,
//! and *"when did this side start trusting that root"* is a question a dispute
//! two years later asks. A window that covers no instant is refused at
//! construction rather than read as *"never valid"*, because a registry entry
//! nobody can use reads exactly like a registry entry nobody made.
//!
//! Note what this window is **not**. The certificate's own `notBefore` and
//! `notAfter` are checked by the path validation, over every certificate on the
//! path — `[OPCP §Provisioning Certificate Pool]` calls that the *validity
//! shell model* and names it as its second control step. The window here is
//! this side's own decision about an anchor it configured, which is a different
//! fact and can be narrower.

use std::collections::BTreeMap;

use iso15118::pnc::pki::{Certificate, Profile, Validated};

use crate::verify::Curves;
use crate::{PncError, RevocationList, Standing};

/// The name of a Plug & Charge ecosystem this platform is connected to.
///
/// A string rather than an enumeration, because the set is an operator's
/// contract list and not a fact about the protocol: `hubject-ccp`,
/// `hubject-rcp`, a national scheme, an OEM's own. R10's *"a contract
/// references a pool, not the pool"* is the whole design and the type is the
/// thinnest thing that carries it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct PoolId(String);

impl PoolId {
    /// Name a pool.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for PoolId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for PoolId {
    fn from(name: &str) -> Self {
        Self::new(name)
    }
}

/// One root this side has decided to trust, and when it decided.
///
/// The window is **private**, and that is the whole of why this type exists
/// rather than a tuple. [`Self::between`] refuses one that covers no instant,
/// and a public pair of fields would make that refusal a door every caller —
/// and every `derive(Deserialize)` — walks around. The rule this workspace
/// keeps re-learning (D264): ask of every invariant which constructors can be
/// bypassed to reach the value it guards.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TrustAnchor {
    /// The certificate, DER.
    pub der: Vec<u8>,
    /// How this side came by it — the sentence a dispute reads.
    ///
    /// `[OPCP §Root Certificate Pool]` makes the delivery of a root an
    /// **organisational** process — *"signed email, SFTP, OFTP2 or similar"* —
    /// and an anchor with no account of which of those it arrived by is an
    /// anchor nobody can defend.
    pub provenance: String,
    /// The first instant this side accepts it. `None` for *"as long as this
    /// registry has existed"*.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339::option"))]
    from: Option<time::OffsetDateTime>,
    /// The first instant it is no longer accepted, exclusive. `None` for open.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339::option"))]
    until: Option<time::OffsetDateTime>,
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for TrustAnchor {
    /// Through [`TrustAnchor::between`], because a derive would restore a
    /// window nobody can be inside — which reads exactly like an anchor nobody
    /// configured, and has a completely different fix.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Wire {
            der: Vec<u8>,
            provenance: String,
            #[serde(with = "time::serde::rfc3339::option")]
            from: Option<time::OffsetDateTime>,
            #[serde(with = "time::serde::rfc3339::option")]
            until: Option<time::OffsetDateTime>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::between(wire.der, wire.provenance, wire.from, wire.until)
            .map_err(serde::de::Error::custom)
    }
}

impl TrustAnchor {
    /// An anchor this side trusts for as long as the registry holds it.
    #[must_use]
    pub fn unbounded(der: impl Into<Vec<u8>>, provenance: impl Into<String>) -> Self {
        Self {
            der: der.into(),
            from: None,
            until: None,
            provenance: provenance.into(),
        }
    }

    /// The same, inside a half-open window.
    ///
    /// # Errors
    ///
    /// [`PncError::EmptyAnchorWindow`] where the window covers no instant.
    pub fn between(
        der: impl Into<Vec<u8>>,
        provenance: impl Into<String>,
        from: Option<time::OffsetDateTime>,
        until: Option<time::OffsetDateTime>,
    ) -> Result<Self, PncError> {
        if let (Some(from), Some(until)) = (from, until)
            && from >= until
        {
            return Err(PncError::EmptyAnchorWindow { from, until });
        }
        Ok(Self {
            der: der.into(),
            from,
            until,
            provenance: provenance.into(),
        })
    }

    /// The first instant this side accepts it, where there is one.
    #[must_use]
    pub const fn from(&self) -> Option<time::OffsetDateTime> {
        self.from
    }

    /// The first instant it is no longer accepted, where there is one.
    #[must_use]
    pub const fn until(&self) -> Option<time::OffsetDateTime> {
        self.until
    }

    /// Whether this side trusted the anchor at an instant.
    #[must_use]
    pub fn trusted_at(&self, at: time::OffsetDateTime) -> bool {
        self.from.is_none_or(|from| at >= from) && self.until.is_none_or(|until| at < until)
    }
}

/// One Plug & Charge ecosystem, and the roots it publishes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Pool {
    anchors: Vec<TrustAnchor>,
}

impl Pool {
    /// A pool with no anchors yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a root this pool publishes.
    #[must_use]
    pub fn with(mut self, anchor: TrustAnchor) -> Self {
        self.anchors.push(anchor);
        self
    }

    /// The anchors this side trusted at an instant.
    pub fn anchors_at(&self, at: time::OffsetDateTime) -> impl Iterator<Item = &TrustAnchor> {
        self.anchors.iter().filter(move |a| a.trusted_at(at))
    }

    /// Every anchor, whether or not its window is open.
    #[must_use]
    pub fn anchors(&self) -> &[TrustAnchor] {
        &self.anchors
    }
}

/// Every pool this platform is connected to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PoolRegistry {
    pools: BTreeMap<String, Pool>,
}

impl PoolRegistry {
    /// A registry connected to nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Connect a pool.
    #[must_use]
    pub fn with(mut self, id: impl Into<PoolId>, pool: Pool) -> Self {
        self.pools.insert(id.into().0, pool);
        self
    }

    /// One pool, by name.
    #[must_use]
    pub fn pool(&self, id: &PoolId) -> Option<&Pool> {
        self.pools.get(&id.0)
    }

    /// The pools this platform is connected to, in a stable order.
    pub fn pools(&self) -> impl Iterator<Item = PoolId> {
        self.pools.keys().map(|name| PoolId(name.clone()))
    }

    /// Validate a certificate chain **inside one pool**, and nowhere else.
    ///
    /// The one door a chain reaches an anchor through, and the reason this type
    /// exists: see the module documentation for what a global trust store does
    /// to `[V2G2-925]` the moment there are two ecosystems.
    ///
    /// `chain` is the leaf followed by its intermediates, leaf first, as DER —
    /// the order a `CertificateChainType` carries on the wire. `at` is the
    /// instant to judge against; this crate reads no clock, and a replay two
    /// years later has to be able to ask the question as it was asked then.
    ///
    /// # What this establishes, and what it does not
    ///
    /// It establishes an unbroken path of signatures to a root **this pool
    /// publishes and this side trusted at `at`**, with every certificate inside
    /// its own validity and every one meeting its Annex F profile.
    ///
    /// It does not establish that anything on the path is unrevoked. That is
    /// [`Standing`], it is a separate argument, and `revocations` is where the
    /// caller supplies it — see [`crate::revocation`] for why it cannot be
    /// folded in here.
    ///
    /// # Errors
    ///
    /// [`PncError::UnknownPool`] for a pool nobody connected — never a fallback
    /// to another one's anchors. [`PncError::NoTrustedAnchor`] where the pool
    /// publishes roots and this side trusted none of them at `at`, which is a
    /// configuration fault and not a bad certificate. [`PncError::Chain`] for
    /// everything the path validation refuses, and [`PncError::Revoked`] where
    /// the supplied status says so.
    pub fn validate_in<'a>(
        &'a self,
        pool: &PoolId,
        chain: &[&'a [u8]],
        profile: Profile,
        at: time::OffsetDateTime,
        revocations: &RevocationList,
    ) -> Result<Validated<'a>, PncError> {
        let connected = self.pool(pool).ok_or_else(|| PncError::UnknownPool {
            pool: pool.clone(),
            connected: self.pools().map(|p| p.0).collect(),
        })?;

        let anchors: Vec<&[u8]> = connected
            .anchors_at(at)
            .map(|anchor| anchor.der.as_slice())
            .collect();
        if anchors.is_empty() {
            return Err(PncError::NoTrustedAnchor {
                pool: pool.clone(),
                at,
                held: connected.anchors.len(),
            });
        }

        let validated =
            iso15118::pnc::pki::validate(chain, &anchors, profile, at.unix_timestamp(), &Curves)
                .map_err(|refusal| PncError::Chain {
                    pool: pool.clone(),
                    refusal,
                })?;

        // …and only now the question the path validation leaves to its caller.
        //
        // Over the **whole** path, leaf and intermediates alike, because that is
        // what `[OPCP §Provisioning Certificate Pool]`'s third control step
        // asks: *"verifies the certificate status of each delivered certificate
        // (leaf and chain) from it's OCSP responder or CRL"*. A check that
        // looked only at the leaf would pass a contract under a sub-CA the
        // ecosystem withdrew this morning.
        let path = core::iter::once(&validated.leaf).chain(validated.intermediates.iter());
        for certificate in path {
            match revocations.standing_of(certificate, at) {
                Standing::Good { .. } => {}
                other => {
                    return Err(PncError::Revoked {
                        subject: subject_of(certificate),
                        standing: Box::new(other),
                    });
                }
            }
        }
        Ok(validated)
    }
}

/// A certificate's subject, for a message a person reads.
fn subject_of(certificate: &Certificate<'_>) -> String {
    certificate
        .subject
        .common_name
        .unwrap_or("a certificate with no Common Name")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn an_anchor_window_that_covers_no_instant_is_refused_at_construction() {
        // The `KeyRegistry` rule, one PKI out: a window nobody can be inside
        // reads exactly like an anchor nobody configured, and the two have
        // different fixes.
        let error = TrustAnchor::between(
            vec![0x30],
            "signed email, 2026-01",
            Some(datetime!(2026-06-01 00:00 UTC)),
            Some(datetime!(2026-01-01 00:00 UTC)),
        )
        .expect_err("a reversed window");
        assert!(
            matches!(error, PncError::EmptyAnchorWindow { .. }),
            "{error}"
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn nothing_reaches_an_anchor_window_past_its_own_constructor() {
        // The rule this workspace keeps re-learning (D264). `between` refuses a
        // window nobody can be inside; a derived deserialiser would restore one
        // from a store, an outbox or an operator's configuration file, and the
        // registry would then hold a root that can never be reached — which
        // reads exactly like a root nobody loaded.
        let good = r#"{"der":[48],"provenance":"OFTP2","from":"2026-01-01T00:00:00Z","until":"2027-01-01T00:00:00Z"}"#;
        let anchor: TrustAnchor = serde_json::from_str(good).expect("a window with an inside");
        assert_eq!(anchor.from(), Some(datetime!(2026-01-01 00:00 UTC)));

        let reversed = r#"{"der":[48],"provenance":"OFTP2","from":"2027-01-01T00:00:00Z","until":"2026-01-01T00:00:00Z"}"#;
        assert!(
            serde_json::from_str::<TrustAnchor>(reversed).is_err(),
            "a derive would have restored this one"
        );
    }

    #[test]
    fn an_anchor_is_trusted_inside_its_own_window_and_not_outside_it() {
        let anchor = TrustAnchor::between(
            vec![0x30],
            "OFTP2, 2026-01",
            Some(datetime!(2026-01-01 00:00 UTC)),
            Some(datetime!(2027-01-01 00:00 UTC)),
        )
        .expect("a window with an inside");
        assert!(!anchor.trusted_at(datetime!(2025-12-31 23:59 UTC)));
        assert!(anchor.trusted_at(datetime!(2026-06-01 00:00 UTC)));
        // Half-open at the far end: the first instant outside is outside.
        assert!(!anchor.trusted_at(datetime!(2027-01-01 00:00 UTC)));
    }

    #[test]
    fn a_pool_nobody_connected_names_the_ones_that_are() {
        let registry = PoolRegistry::new().with("hubject-ccp", Pool::new());
        let error = registry
            .validate_in(
                &PoolId::new("some-other-ecosystem"),
                &[&[0x30][..]],
                Profile::ContractCertificate,
                datetime!(2026-06-01 00:00 UTC),
                &RevocationList::default(),
            )
            .expect_err("no such pool");
        assert!(
            matches!(&error, PncError::UnknownPool { connected, .. } if connected == &["hubject-ccp".to_owned()]),
            "{error}"
        );
    }

    #[test]
    fn a_pool_whose_anchors_are_all_outside_their_windows_is_a_configuration_fault() {
        // Distinguished from a bad certificate on purpose: the remedy is to
        // load a root, not to go and ask a driver for another card.
        let pool = Pool::new().with(
            TrustAnchor::between(
                vec![0x30],
                "expired trust decision",
                None,
                Some(datetime!(2026-01-01 00:00 UTC)),
            )
            .expect("a window with an inside"),
        );
        let registry = PoolRegistry::new().with("hubject-ccp", pool);
        let error = registry
            .validate_in(
                &PoolId::new("hubject-ccp"),
                &[&[0x30][..]],
                Profile::ContractCertificate,
                datetime!(2026-06-01 00:00 UTC),
                &RevocationList::default(),
            )
            .expect_err("no anchor is trusted at that instant");
        assert!(
            matches!(error, PncError::NoTrustedAnchor { held: 1, .. }),
            "{error}"
        );
    }
}
