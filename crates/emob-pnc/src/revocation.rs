//! Whether a certificate on a validated path has been withdrawn — the question
//! the path validation leaves to its caller, and this crate's reason to exist
//! beside it.
//!
//! # Why the kit does not answer it, and why this does
//!
//! `iso15118::pnc::pki::validate` says so in as many words: it establishes that
//! the leaf's key is certified by an unbroken chain to a trusted anchor, and
//! *"that no certificate on the path has been **revoked**"* is what it does
//! not. There is no OCSP client and no CRL fetcher in a `no_std` crate that
//! opens no sockets, and there should not be.
//!
//! `[OPCP §Provisioning Certificate Pool]` makes it a control step all the
//! same, and makes it the **third** of four:
//!
//! > 3. Verifies the certificate status of each delivered certificate (leaf and
//! >    chain) from it's OCSP responder or CRL.
//!
//! Note *"leaf and chain"*. A check that asks only about the contract
//! certificate passes a contract issued under a sub-CA the ecosystem withdrew
//! this morning, which is the one revocation that matters most: a leaf is one
//! driver and a sub-CA is every driver under it.
//!
//! # Three answers, not two
//!
//! The shape this crate refuses is a `bool`. *Revoked* and *not revoked* leaves
//! nowhere to put the case that actually happens — **nobody could say** — and a
//! platform whose type has two states puts that third case in whichever of them
//! the code was written by. It ends up in *not revoked*, every time, because
//! that is the answer that lets the session start.
//!
//! So [`Standing`] has four, and only one of them authorises anything:
//!
//! * [`Standing::Good`] — a responder said so, recently enough to mean it;
//! * [`Standing::Revoked`] — a responder said so;
//! * [`Standing::Unknown`] — this side holds no status for the certificate at
//!   all, which is not the same as good and must not read as it;
//! * [`Standing::Stale`] — this side holds one and it is older than the
//!   operator will vouch for, which is not the same as either.
//!
//! That is the same sentence `emob_tariff` says about an element carrying a
//! restriction it cannot judge, and `emob_service::policy` says about a Cedar
//! policy that errors: **what cannot be evaluated is not assumed open.**
//!
//! # A status is a claim about an instant
//!
//! A [`RevocationList`] carries the instant it was compiled, and a freshness
//! bound is an argument. Both are the same discipline as `poid::stale` and
//! `emob_eichrecht::KeyRegistry`: published data fails by going quietly out of
//! date, nothing errors, and the map is simply wrong. A list from March,
//! consulted in September, is not evidence that a contract cancelled in June is
//! still good — and a platform that reads it as one has an OCSP client that is
//! decorative.
//!
//! A withdrawn **root** deletes nothing: a finding here refuses *this*
//! validation and names the certificate it was about. See [`crate::contract`]
//! for which direction the one cascade `[OPCP]` describes actually runs in.

use std::collections::BTreeMap;

use iso15118::pnc::pki::Certificate;

/// What this side can say about one certificate's standing.
///
/// See the module documentation for why there are four of these and not two.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Standing {
    /// A status this side holds, fresh enough to act on, that does not list it.
    Good {
        /// When the status was compiled.
        #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
        as_of: time::OffsetDateTime,
    },
    /// A status this side holds lists it as withdrawn.
    Revoked {
        /// When the status was compiled.
        #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
        as_of: time::OffsetDateTime,
        /// What the responder gave as the reason, where it gave one.
        reason: Option<String>,
    },
    /// This side holds no status for the issuer at all.
    ///
    /// Not *"good"*. An issuer nobody has a responder for is an issuer this
    /// platform cannot say anything about, and saying nothing is the honest
    /// answer.
    Unknown,
    /// This side holds a status and it is older than the caller will vouch for.
    ///
    /// The failure mode of published data, one PKI along: nothing errors, the
    /// file parses, and the answer is about a world that has moved.
    Stale {
        /// When the status was compiled.
        #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
        as_of: time::OffsetDateTime,
        /// How far past the freshness bound it is.
        #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::duration_seconds"))]
        overdue_by: time::Duration,
    },
}

impl Standing {
    /// Whether this standing authorises anything. Only [`Self::Good`] does.
    #[must_use]
    pub const fn is_good(&self) -> bool {
        matches!(self, Self::Good { .. })
    }
}

impl core::fmt::Display for Standing {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Good { as_of } => write!(f, "not withdrawn, on a status compiled at {as_of}"),
            Self::Revoked {
                as_of,
                reason: Some(reason),
            } => write!(f, "withdrawn ({reason}), on a status compiled at {as_of}"),
            Self::Revoked {
                as_of,
                reason: None,
            } => {
                write!(f, "withdrawn, on a status compiled at {as_of}")
            }
            Self::Unknown => f.write_str(
                "no revocation status is held for its issuer at all: a certificate nobody can \
                 vouch for is not one to read as good [OPCP §Provisioning Certificate Pool]",
            ),
            Self::Stale { as_of, overdue_by } => write!(
                f,
                "the only status held for its issuer was compiled at {as_of}, {} minutes past \
                 what this side vouches for: a stale list is not evidence that a contract \
                 cancelled since is still good",
                overdue_by.whole_minutes()
            ),
        }
    }
}

/// The withdrawals one issuer has published, and when it published them.
///
/// Per **issuer**, because that is how a CRL is scoped and how an OCSP
/// responder is addressed: a serial number is unique inside one issuer's
/// namespace and means nothing outside it. Keyed on the issuer's distinguished
/// name **as the certificate encodes it** — the DER, not a rendering of it —
/// because two encodings of one name are one issuer and a rendering is a place
/// for them to stop being.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Withdrawals {
    /// When this status was compiled — a status is a claim about an instant.
    #[cfg_attr(feature = "serde", serde(with = "time::serde::rfc3339"))]
    pub as_of: time::OffsetDateTime,
    /// The serial numbers this issuer has withdrawn, as the certificates
    /// encode them, each with the reason where the responder gave one.
    pub revoked: BTreeMap<Vec<u8>, Option<String>>,
}

impl Withdrawals {
    /// A status compiled at an instant, listing nothing.
    #[must_use]
    pub fn none_at(as_of: time::OffsetDateTime) -> Self {
        Self {
            as_of,
            revoked: BTreeMap::new(),
        }
    }

    /// …and one serial it lists.
    #[must_use]
    pub fn and(mut self, serial: impl Into<Vec<u8>>, reason: Option<String>) -> Self {
        self.revoked.insert(serial.into(), reason);
        self
    }
}

/// Every issuer this side holds a status for, and how fresh a status has to be.
///
/// The default holds nothing, which makes every answer [`Standing::Unknown`]
/// and every validation fail. That is the correct default and it is a loud one:
/// a platform is not doing Plug & Charge until somebody has connected it to a
/// responder, and a type whose default silently authorised everything would let
/// that never happen.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RevocationList {
    by_issuer: BTreeMap<Vec<u8>, Withdrawals>,
    /// How old a status may be and still be acted on.
    #[cfg_attr(feature = "serde", serde(with = "emob_core::wire::duration_seconds"))]
    fresh_for: time::Duration,
}

/// How old a status may be by default before it stops being evidence.
///
/// Twenty-four hours, and it is a **default rather than a rule**: how often an
/// ecosystem's responder is polled is an operating decision, `[OPCP]` states no
/// number, and a constant here presented as a requirement would be a citation
/// this crate cannot support. [`RevocationList::fresh_for`] is where an operator
/// says what it actually holds itself to.
pub const DEFAULT_FRESHNESS: time::Duration = time::Duration::hours(24);

impl Default for RevocationList {
    fn default() -> Self {
        Self {
            by_issuer: BTreeMap::new(),
            fresh_for: DEFAULT_FRESHNESS,
        }
    }
}

impl RevocationList {
    /// A list holding no status for anybody.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How old a status may be and still be acted on.
    #[must_use]
    pub const fn fresh_for(mut self, window: time::Duration) -> Self {
        self.fresh_for = window;
        self
    }

    /// Take on one issuer's published status.
    ///
    /// `issuer_dn` is the issuer's distinguished name as DER —
    /// `certificate.subject.encoded` of the issuing certificate, or
    /// `certificate.issuer.encoded` of one it issued, which are the same bytes.
    #[must_use]
    pub fn with(mut self, issuer_dn: impl Into<Vec<u8>>, withdrawals: Withdrawals) -> Self {
        self.by_issuer.insert(issuer_dn.into(), withdrawals);
        self
    }

    /// What this side can say about one certificate at an instant.
    ///
    /// Self-issued certificates are asked the same question as any other. A
    /// root that has been withdrawn is the case `[OPCP §Root Certificate Pool]`
    /// spends a paragraph on, and skipping it because *"a root vouches for
    /// itself"* is how it goes unnoticed.
    #[must_use]
    pub fn standing_of(&self, certificate: &Certificate<'_>, at: time::OffsetDateTime) -> Standing {
        let Some(status) = self.by_issuer.get(certificate.issuer.encoded) else {
            return Standing::Unknown;
        };
        let age = at - status.as_of;
        if age > self.fresh_for {
            return Standing::Stale {
                as_of: status.as_of,
                overdue_by: age - self.fresh_for,
            };
        }
        match status.revoked.get(certificate.serial) {
            Some(reason) => Standing::Revoked {
                as_of: status.as_of,
                reason: reason.clone(),
            },
            None => Standing::Good {
                as_of: status.as_of,
            },
        }
    }

    /// The issuers this side holds a status for, in a stable order.
    pub fn issuers(&self) -> impl Iterator<Item = &[u8]> {
        self.by_issuer.keys().map(Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn at(hours: i64) -> time::OffsetDateTime {
        datetime!(2026-06-01 00:00 UTC) + time::Duration::hours(hours)
    }

    // `standing_of` takes a parsed `Certificate`, so it is exercised where a
    // real one exists: `tests/a_contract_is_installed.rs`, against Hubject's
    // own published test PKI. What is asserted here is the part that is a
    // value — which standings authorise, and what each one says.

    #[test]
    fn only_good_authorises() {
        // The whole reason there are four of these. A `bool` would have put the
        // two middle cases in whichever half the code was written by, and it is
        // always the half that lets the session start.
        assert!(Standing::Good { as_of: at(0) }.is_good());
        assert!(!Standing::Unknown.is_good());
        assert!(
            !Standing::Stale {
                as_of: at(0),
                overdue_by: time::Duration::hours(1),
            }
            .is_good()
        );
        assert!(
            !Standing::Revoked {
                as_of: at(0),
                reason: Some("keyCompromise".to_owned()),
            }
            .is_good()
        );
    }

    #[test]
    fn every_standing_says_what_it_means_in_a_sentence() {
        assert!(Standing::Unknown.to_string().contains("[OPCP"));
        assert!(
            Standing::Stale {
                as_of: at(0),
                overdue_by: time::Duration::minutes(90),
            }
            .to_string()
            .contains("90 minutes")
        );
        assert!(
            Standing::Revoked {
                as_of: at(0),
                reason: Some("keyCompromise".to_owned()),
            }
            .to_string()
            .contains("keyCompromise")
        );
    }

    #[test]
    fn the_freshness_window_is_the_operators_and_the_constant_is_only_a_default() {
        assert_eq!(
            RevocationList::new().fresh_for,
            DEFAULT_FRESHNESS,
            "a default rather than a rule: `[OPCP]` states no number"
        );
        assert_eq!(
            RevocationList::new()
                .fresh_for(time::Duration::minutes(15))
                .fresh_for,
            time::Duration::minutes(15)
        );
    }

    #[test]
    fn a_list_holding_nothing_is_the_default_and_it_authorises_nobody() {
        // Loud rather than convenient: a platform is not doing Plug & Charge
        // until somebody has connected it to a responder, and a default that
        // quietly said `Good` would let that never happen.
        assert_eq!(RevocationList::new().issuers().count(), 0);
    }

    #[test]
    fn a_status_is_scoped_to_one_issuer_because_a_serial_is() {
        let issuer = b"MO Sub CA 2 DN".to_vec();
        let list = RevocationList::new().with(
            issuer.clone(),
            Withdrawals::none_at(at(0))
                .and(vec![0x01, 0x02], Some("cessationOfOperation".to_owned())),
        );
        assert_eq!(list.issuers().collect::<Vec<_>>(), vec![issuer.as_slice()]);
    }
}
