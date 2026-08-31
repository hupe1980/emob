//! How a charging session was authorised.
//!
//! Five paths reach the same outcome — a station starts delivering energy — and
//! they are not interchangeable. Two things depend on which one was used:
//!
//! - **AFIR.** A publicly accessible point must offer at least one path that
//!   needs no contract `[AFIR Art. 5(1)]`, and the price on that path carries
//!   no roaming surcharge `[AFIR Art. 5(4)]`.
//! - **Eichrecht.** The signed record states how strongly the user was
//!   identified `[OCMF Tab. 11]`. A session that *claims* Plug & Charge and
//!   whose signed record says a bare RFID UID was read is telling two different
//!   stories, and [`AuthPath::strongest_plausible_level`] is what lets the CDR
//!   layer notice.

use emob_core::{ContractId, Emaid};

/// The mechanism that authorised a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum AuthPath {
    /// The station's own local authorisation list or cache decided.
    LocalList,
    /// An `Authorize` went to the e-mobility provider, directly or through a
    /// roaming hub, and came back affirmative.
    Roaming,
    /// No contract at all: a payment card at the point, or a web/QR flow.
    /// The path AFIR requires every public point to offer.
    AdHoc,
    /// ISO 15118 Plug & Charge: the vehicle presented a contract certificate.
    PlugAndCharge,
    /// The vehicle was recognised by its MAC address.
    ///
    /// Not a standard, not authenticated, and trivially spoofable — it is
    /// modelled because the field uses it, and kept distinct from
    /// [`Self::PlugAndCharge`] because the two are constantly conflated in
    /// marketing and must never be conflated in evidence.
    AutoCharge,
    /// A backend command (`RemoteStartTransaction`, `StartSession`) began it.
    RemoteCommand,
}

impl AuthPath {
    /// Whether this path works without the driver holding a contract.
    ///
    /// The AFIR Art. 5(1) question, asked of a session rather than of a
    /// station.
    #[must_use]
    pub const fn is_contract_free(self) -> bool {
        matches!(self, Self::AdHoc)
    }

    /// Whether the driver was identified cryptographically.
    #[must_use]
    pub const fn is_cryptographic(self) -> bool {
        matches!(self, Self::PlugAndCharge)
    }

    /// The strongest OCMF identification level this path can honestly support.
    ///
    /// Used to cross-check a session against its own signed records: a session
    /// claiming [`Self::PlugAndCharge`] whose record reports `RFID_PLAIN` is
    /// two stories about one event, and the weaker one is the one with a
    /// signature behind it.
    ///
    /// The mapping is deliberately generous — it is a ceiling, not an
    /// expectation — because a station may under-report and that is not a
    /// fault. Over-reporting is.
    #[must_use]
    #[allow(
        clippy::match_same_arms,
        reason = "the arms coincide today but hold for different reasons, and each \
                  reason is what a reviewer has to check against OCMF Table 11; \
                  merging them would delete the argument and make the next change silent"
    )]
    pub const fn strongest_plausible_level(self) -> IdentificationStrength {
        match self {
            // A contract certificate verified by the station.
            Self::PlugAndCharge => IdentificationStrength::Secure,
            // The backend vouched for it; the station took its word.
            Self::Roaming | Self::RemoteCommand => IdentificationStrength::Trusted,
            // A payment instrument, but not one the station verified
            // cryptographically against a contract.
            Self::AdHoc => IdentificationStrength::Trusted,
            // A UID read from a card, or a MAC address off the wire.
            Self::LocalList | Self::AutoCharge => IdentificationStrength::Hearsay,
        }
    }
}

impl core::fmt::Display for AuthPath {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::LocalList => "local authorisation list",
            Self::Roaming => "roaming authorisation",
            Self::AdHoc => "ad-hoc",
            Self::PlugAndCharge => "Plug & Charge",
            Self::AutoCharge => "AutoCharge",
            Self::RemoteCommand => "remote command",
        })
    }
}

/// How strongly a user was tied to a session, ordered.
///
/// A coarsening of `[OCMF Tab. 11]`'s levels that drops the error states, so
/// that comparing two strengths is always meaningful. The OCMF error levels
/// (`MISMATCH`, `INVALID`, `OUTDATED`, `UNKNOWN`) are not weak assignments —
/// they are failures, and they are handled where they belong, in
/// `emob-eichrecht`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum IdentificationStrength {
    /// No assignment at all.
    None,
    /// Unsecured — a bare RFID UID, a MAC address.
    Hearsay,
    /// A backend vouched for it.
    Trusted,
    /// The signature component verified it by special measures.
    Verified,
    /// A cryptographic signature certifies the assignment.
    Certified,
    /// A secure feature established it — a secure card, Plug & Charge.
    Secure,
}

impl core::fmt::Display for IdentificationStrength {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Hearsay => "hearsay",
            Self::Trusted => "trusted",
            Self::Verified => "verified",
            Self::Certified => "certified",
            Self::Secure => "secure",
        })
    }
}

/// Who the session is for.
///
/// An ad-hoc session has no contract by construction, which is why this is an
/// enum rather than an `Option<ContractId>`: "no contract" and "contract not
/// recorded" are different facts, and only the first one is lawful to bill
/// against an ad-hoc tariff.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum Subject {
    /// A contract at an e-mobility provider.
    Contract {
        /// The provider-side contract.
        id: ContractId,
        /// The contract identifier the vehicle or token presented, when there
        /// is one.
        emaid: Option<Emaid>,
    },
    /// A driver with no contract, paying at the point.
    AdHoc,
}

impl Subject {
    /// The contract, if there is one.
    #[must_use]
    pub const fn contract_id(&self) -> Option<&ContractId> {
        match self {
            Self::Contract { id, .. } => Some(id),
            Self::AdHoc => None,
        }
    }
}

/// The complete authorisation of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Authorization {
    /// Which mechanism.
    pub path: AuthPath,
    /// Who for.
    pub subject: Subject,
    /// An opaque, keyed hash of the token that was presented.
    ///
    /// Never the RFID UID or the eMAID itself. A UID is a lifelong identifier
    /// of a physical object a person carries; storing it in every session row
    /// builds a movement profile that no part of this platform needs.
    pub token_ref: Option<TokenRef>,
    /// The provider's authorisation reference, for a roaming session.
    pub authorization_reference: Option<String>,
}

impl Authorization {
    /// An ad-hoc authorisation: no contract, by construction.
    #[must_use]
    pub const fn ad_hoc() -> Self {
        Self {
            path: AuthPath::AdHoc,
            subject: Subject::AdHoc,
            token_ref: None,
            authorization_reference: None,
        }
    }

    /// Whether this session may be billed against an ad-hoc tariff.
    ///
    /// Both halves have to agree. A session on the ad-hoc *path* that carries a
    /// contract is a contract session that used a card reader, and billing it
    /// at the ad-hoc price would be charging a customer twice over.
    #[must_use]
    pub fn is_ad_hoc(&self) -> bool {
        self.path.is_contract_free() && matches!(self.subject, Subject::AdHoc)
    }
}

/// An opaque reference to a token, safe to store.
///
/// Constructed by hashing the token with a key the platform holds — the
/// construction itself lives in a service, because it needs the key and this
/// crate does no I/O. What is enforced here is that the type cannot be built
/// from something that merely *looks* like a hash: a caller who passes a raw
/// UID gets a refusal rather than a privacy incident.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct TokenRef(String);

impl TokenRef {
    /// The length a reference must have: 64 lowercase hex characters, i.e. a
    /// 256-bit digest.
    pub const HEX_LEN: usize = 64;

    /// Wrap a keyed digest.
    ///
    /// # Errors
    ///
    /// [`AuthError::NotADigest`] unless the value is exactly 64 lowercase hex
    /// characters. An RFID UID is 8 or 14 hex characters and an eMAID is 14 or
    /// 15 alphanumerics, so neither can pass — which is the point.
    pub fn new(digest_hex: impl Into<String>) -> Result<Self, AuthError> {
        let value = digest_hex.into();
        if value.len() != Self::HEX_LEN
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(AuthError::NotADigest { len: value.len() });
        }
        Ok(Self(value))
    }

    /// The digest as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What can be wrong with an authorisation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// The value is not a 256-bit hex digest — very possibly a raw token.
    #[error(
        "a token reference is {} lowercase hex characters (a keyed digest), got {len}; never store a raw UID or eMAID",
        TokenRef::HEX_LEN
    )]
    NotADigest {
        /// The length that was offered.
        len: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ad_hoc_is_contract_free() {
        assert!(AuthPath::AdHoc.is_contract_free());
        for path in [
            AuthPath::LocalList,
            AuthPath::Roaming,
            AuthPath::PlugAndCharge,
            AuthPath::AutoCharge,
            AuthPath::RemoteCommand,
        ] {
            assert!(!path.is_contract_free());
        }
    }

    #[test]
    fn autocharge_is_not_plug_and_charge() {
        // Conflated everywhere in marketing; never in evidence.
        assert!(AuthPath::PlugAndCharge.is_cryptographic());
        assert!(!AuthPath::AutoCharge.is_cryptographic());
        assert!(
            AuthPath::AutoCharge.strongest_plausible_level() < IdentificationStrength::Trusted,
            "a MAC address is hearsay"
        );
    }

    #[test]
    fn paths_and_strengths_render_as_prose() {
        assert_eq!(AuthPath::AdHoc.to_string(), "ad-hoc");
        assert_eq!(AuthPath::PlugAndCharge.to_string(), "Plug & Charge");
        assert_eq!(IdentificationStrength::Secure.to_string(), "secure");
    }

    #[test]
    fn identification_strength_is_ordered() {
        assert!(IdentificationStrength::Secure > IdentificationStrength::Trusted);
        assert!(IdentificationStrength::Trusted > IdentificationStrength::Hearsay);
        assert!(IdentificationStrength::Hearsay > IdentificationStrength::None);
    }

    #[test]
    fn an_ad_hoc_session_needs_both_halves() {
        let ad_hoc = Authorization::ad_hoc();
        assert!(ad_hoc.is_ad_hoc());

        // The ad-hoc path with a contract behind it is a contract session that
        // happened to use the card reader.
        let mixed = Authorization {
            path: AuthPath::AdHoc,
            subject: Subject::Contract {
                id: "c-1".parse().unwrap(),
                emaid: None,
            },
            token_ref: None,
            authorization_reference: None,
        };
        assert!(!mixed.is_ad_hoc());
    }

    #[test]
    fn a_token_ref_refuses_a_raw_identifier() {
        // An RFID UID…
        assert!(TokenRef::new("1F2D3A4F5506C7").is_err());
        // …an eMAID…
        assert!(TokenRef::new("DE8AACA2B3C4D51").is_err());
        // …and anything that is not a 256-bit digest.
        assert!(TokenRef::new("").is_err());
        assert!(
            TokenRef::new("A".repeat(64)).is_err(),
            "uppercase is not the canonical form"
        );

        let digest = "a".repeat(64);
        assert_eq!(TokenRef::new(digest.clone()).unwrap().as_str(), digest);
    }

    #[test]
    fn the_refusal_says_what_to_do_instead() {
        let err = TokenRef::new("1F2D3A4F5506C7").unwrap_err();
        assert!(err.to_string().contains("never store a raw UID"));
    }
}
